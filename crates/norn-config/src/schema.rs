//! The vault schema's content model — what a vault declares about itself.
//!
//! A vault schema is a YAML file the author writes and norn never edits. Until
//! this module existed it was bytes: read, hashed, pinned, and opaque to every
//! consumer. [`VaultSchema`] is the typed reading of those bytes — the declared
//! fields with their types and shapes, the declared tag facet, the path rules,
//! the schema rules that constrain the documents they select, and the creation
//! rules and inbox that say how a new document is made — and it is what makes
//! a declaration something
//! derivation and the read surface can act on.
//!
//! **The model is a pure function of the bytes, and its identity is the schema
//! fingerprint.** Nothing here reads a file, a clock or an environment: a
//! caller hands over the bytes it pinned and gets the model those bytes mean.
//! Two callers holding one fingerprint therefore hold one model, which is what
//! lets derived state be keyed by the fingerprint alone and lets a re-pin be
//! the whole of a schema change.
//!
//! # The shape a schema is written in
//!
//! ```yaml
//! version: 1
//! fields:
//!   title:
//!     type: text
//!   created:
//!     type: date
//!   status:
//!     type: text
//!     shape: single
//! tags:
//!   declared: [project, area]
//!   patterns: ["person/**"]
//!   undeclared: report
//! paths:
//!   ambiguity_ignore: ["archive/**"]
//! rules:
//!   task:
//!     description: A tracked task
//!     severity: error
//!     match:
//!       frontmatter: { type: [task, chore] }
//!       path: "projects/<project>/**"
//!     required:
//!       status: { default: todo }
//!     one_of:
//!       status: { values: [todo, doing, done], synonyms: { complete: done } }
//!     allowed_paths:
//!       paths: ["projects/*/tasks/**"]
//!       route: "projects/{{path.project}}/tasks/"
//! creatable:
//!   task:
//!     target: "tasks/{{var.project}}-{{seq}}.md"
//!     variables: [project, title]
//!     frontmatter_defaults:
//!       status: todo
//!       created: "{{now}}"
//!     body: "# {{var.title}}\n"
//! inbox:
//!   target: "inbox/{{date}}-{{seq}}.md"
//! ```
//!
//! **A field declaration is a type and a shape, and nothing else.** `type` is
//! one of the five [`FieldType`]s, text where absent; `shape` is `single` or
//! `list`, and either is admitted where it is absent ([`Shape`]). Whether a
//! field is required, which values it holds and where a document may stand
//! are constraints a schema rule states — see [`rules`].
//!
//! **A creation rule is a template, and so is the inbox.** `target`, `body`
//! and every string scalar in `frontmatter_defaults` are written in the
//! grammar [`template`] reads, and where each token may stand is judged when
//! the schema is read — see [`creation`]. The model holds the templates, not
//! what they fill to: a clock reading and a caller's variables are the
//! planner's, so the model stays a function of the bytes.
//!
//! **A tag is compared under the tag fold.** `tags.declared` names and
//! `tags.patterns` match a tag with Unicode case folded and accents kept, over
//! the whole nested name: `declared: [Work]` admits `#work`, `patterns:
//! ["area/**"]` admits `#Area/Work`, and neither admits `#Wörk`. Two declared
//! names that fold to one are one declaration, and so are two patterns, each
//! held at the spelling written first. See [`TagFacet`].
//!
//! Every section is optional. A schema that declares nothing — which is what
//! `version: 1` alone is — is a valid schema that judges no document, and
//! [`VaultSchema::rederives_documents`] answers whether it is worth
//! re-deriving any document under it.
//!
//! **A key this grammar does not hold is a refusal.** `tagz:` or
//! `undecalred: report` would otherwise read as a valid schema that quietly
//! declares nothing — turning a vault's whole reporting posture off with one
//! typo — so an unknown key is refused exactly as a version this build does
//! not read is.
//!
//! # What the model does not express, and why
//!
//! **Graph-relationship constraints are not declarable.** A rule of the form
//! *a document of type X must link to a document of type Y* is evaluated over
//! the resolved link graph, and its truth moves when a document the rule is not
//! about is added, renamed or deleted.
//!
//! Every rule this model declares is a function of one document's own facts
//! plus the schema, so the two keys derivation already maintains — that
//! document's content hash and the schema fingerprint — reach every finding
//! the rule can mint. The findings pillar does carry a third axis for rules
//! whose truth moves with another document: class-scoped maintenance, which is
//! how an ambiguity finding written in one document is revisited when a second
//! document joins or leaves its resolution class. That axis is keyed by a
//! resolution class, and a graph rule's dependency is not one: the documents
//! whose changes can falsify *X links to Y* are those the rule's own link
//! resolved to and those a later edit makes it resolve to instead, which is
//! not the set any suffix class opens.
//!
//! So the refusal is narrow and it is about this model as shipped: nothing
//! here declares a rule whose invalidation the two document-level keys miss,
//! and the ambiguity-ignore set this model *does* declare is not such a rule —
//! it narrows which paths the resolution ladder counts, which is the read
//! surface's to apply, and it mints no finding of its own. A graph rule
//! arrives with the maintenance that revisits it, not before.
//!
//! # Where the model is consumed
//!
//! Derivation reads the tag facet and files the facet's findings, and reads
//! each declared field's [`FieldType`] to fill the typed column the field
//! pillar sorts by. The read surface reads the declared fields to answer the
//! field universe, and reads
//! [`FieldType`] to give one comparison rule to sorts, ranges and comparison
//! operators alike — see [`typed`]. The ambiguity-ignore patterns name the
//! paths the resolution ladder does not count as candidates: derivation hands
//! them to the store with the rest of the content model, the store's resolver
//! applies them wherever a target's class is read, and `describe` reports the
//! same set as path rules. `describe` reports each creation rule and the
//! inbox as facets, templates as their source text; no derivation reads
//! either. Derivation judges every document against the declared fields'
//! types and shapes and the schema rules selecting it, and files what the
//! judgment concludes as findings ([`VaultSchema::judge`]); the rules also
//! judge themselves at schema read, and what consumes them beyond that is
//! stated in [`rules`].

pub mod creation;
pub mod rules;
pub mod template;
pub mod typed;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_yaml::Value;

pub use creation::{CreationProblem, CreationRule, Inbox, SeqSlot, Target};
use norn_wire::fold_tag;
pub use norn_wire::{Binding, Captures, CaseFold, Pattern, PatternError};
pub use rules::{
    AllowedPaths, Breach, ClosedSet, CombinedConstraint, DefaultCandidate, DefaultsConflict,
    ElementProblem, FieldConstraint, ForbiddenFix, Judgment, OneOfIntersection, PLACEMENT_CEILING,
    Route, Rule, RuleDefault, RuleDefaultsRefusal, RuleFinding, RuleProblem, RuleWork,
    RulesConflict, Selector,
};
pub use template::{
    FillError, LocalTimestamp, NotALocalTimestamp, Template, TemplateError, TemplateValues,
    UnsafeValue,
};
pub use typed::{Comparison, ComparisonSignal, DateValue, FieldType, Offset, TypedValue};

/// The schema version this build reads.
///
/// A schema that states another version is refused rather than read under
/// this one's meanings: a declaration written for a grammar this build does
/// not have is not a declaration this build can honour.
pub const SCHEMA_VERSION: i64 = 1;

/// The sections a schema declares, which is every key its root holds.
const ROOT_KEYS: &[&str] = &[
    "version",
    "fields",
    "tags",
    "paths",
    "rules",
    "creatable",
    "inbox",
];

/// The keys one field's declaration holds.
const FIELD_KEYS: &[&str] = &["type", "shape"];

/// The keys the tag facet holds.
const TAG_KEYS: &[&str] = &["declared", "patterns", "undeclared"];

/// The keys the path rules hold.
const PATH_KEYS: &[&str] = &["ambiguity_ignore"];

/// One vault's declaration about itself.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VaultSchema {
    fields: BTreeMap<String, DeclaredField>,
    tags: TagFacet,
    ambiguity_ignore: Vec<Pattern>,
    rules: BTreeMap<String, Rule>,
    creation_rules: BTreeMap<String, CreationRule>,
    inbox: Option<Inbox>,
    /// Rule judgment's memo of placement verdicts, which is no part of the
    /// model: see [`rules::PlacementVerdicts`].
    placement_verdicts: rules::PlacementVerdicts,
}

impl VaultSchema {
    /// Reads schema bytes into the model they mean.
    pub fn parse(bytes: &[u8]) -> Result<Self, VaultSchemaError> {
        let text = std::str::from_utf8(bytes).map_err(|error| VaultSchemaError::NotUtf8 {
            message: error.to_string(),
        })?;
        let document: Value =
            serde_yaml::from_str(text).map_err(|error| VaultSchemaError::NotYaml {
                message: error.to_string(),
            })?;
        let document = match document {
            // An empty file is a document with no content, and a vault that
            // declares nothing is exactly what the default model is.
            Value::Null => return Ok(Self::default()),
            Value::Mapping(mapping) => mapping,
            other => {
                return Err(VaultSchemaError::NotAMapping {
                    found: type_name(&other),
                });
            }
        };
        read_version(&document)?;
        known_keys_only("", &document, ROOT_KEYS)?;
        let fields = read_fields(&document)?;
        let rules = rules::read_rules(&document, &fields)?;
        let schema = VaultSchema {
            fields,
            tags: read_tags(&document)?,
            ambiguity_ignore: read_ambiguity_ignore(&document)?,
            rules,
            creation_rules: creation::read_creatable(&document)?,
            inbox: creation::read_inbox(&document)?,
            placement_verdicts: rules::PlacementVerdicts::default(),
        };
        rules::check_rules(&schema)?;
        Ok(schema)
    }

    /// The declared fields, in key order.
    ///
    /// Read by derivation, which hands the store every declaration and the
    /// typed order each typed field carries: the field pillar's typed column is
    /// filled under this schema, a find reads the declared keys as the declared
    /// half of the field universe it judges a key against, and `describe`
    /// reports each declaration as a facet.
    pub fn fields(&self) -> impl Iterator<Item = (&str, &DeclaredField)> {
        self.fields.iter().map(|(key, field)| (key.as_str(), field))
    }

    /// One field's declaration, if the schema names it.
    pub fn field(&self, key: &str) -> Option<&DeclaredField> {
        self.fields.get(key)
    }

    /// The declared tag facet.
    pub fn tags(&self) -> &TagFacet {
        &self.tags
    }

    /// The paths the resolution ladder does not count as candidates.
    ///
    /// **Read by the resolution ladder, and reported by `describe`.**
    /// Derivation declares the set on the declaration it hands the store,
    /// which holds it once: the store's one resolver applies it to every class
    /// a target opens — a find's `resolves` part, a get's target, a links-to
    /// part's target and the backlinks it matches, and each wikilink a row's
    /// links resolve — and `describe` reports each glob of it as a path rule.
    /// The globs match under the store's recorded path order: with ASCII case
    /// folded on a root that folds it, bytewise on a root that does not.
    ///
    /// Link-health findings read the same exclusion: the store judges each
    /// link a changeset reaches through that one resolver, under the
    /// declaration derivation hands it.
    pub fn ambiguity_ignore(&self) -> &[Pattern] {
        &self.ambiguity_ignore
    }

    /// The creation rules, in the byte order of their names.
    ///
    /// Read by `describe`, which reports each as a facet. The planner reads
    /// one by its name ([`VaultSchema::creation_rule`]) to expand a
    /// create-by-rule operation into the document it makes.
    pub fn creation_rules(&self) -> impl Iterator<Item = &CreationRule> {
        self.creation_rules.values()
    }

    /// The creation rule called `name`, if the schema declares one.
    pub fn creation_rule(&self, name: &str) -> Option<&CreationRule> {
        self.creation_rules.get(name)
    }

    /// Where untyped capture lands, if the schema declares an inbox.
    pub fn inbox(&self) -> Option<&Inbox> {
        self.inbox.as_ref()
    }

    /// Whether a pin of this schema obliges a re-derivation of the documents
    /// standing under it.
    ///
    /// The re-derivation a schema change implies costs the vault, so this is
    /// the question to ask before paying it; no caller asks it yet (see the
    /// dormant carrier below). The answer is the disjunction over
    /// the declarations some per-document derived state reads — state a
    /// document's own re-derivation derives again — and that set holds three:
    ///
    /// - **A tag facet that reports**, whose findings are derived per document.
    /// - **A declared field**, whatever its type and shape. Rule judgment
    ///   files a type or shape mismatch against every declaration, a field
    ///   declared text included — a map reads as no type — and a field whose
    ///   type does not order as text also has its values held in the field
    ///   pillar's typed column. A pin clears that column, so a schema
    ///   declaring one owes every document standing under it the
    ///   re-derivation that refills it, whether or not its bytes moved.
    /// - **A schema rule**, whose findings are judged per document
    ///   ([`VaultSchema::judge`]).
    ///
    /// Creation rules and the inbox are no term: they say how a document is
    /// made, and no row a document's derivation writes reads them.
    ///
    /// A schema declaring none of them leaves every row with the same
    /// per-document derived state under the new pin as under the old. **A
    /// declaration gaining a per-document consumer joins this disjunction in
    /// the same change**: a schema answering `false` here while some
    /// per-document state reads its declaration would leave that state derived
    /// under a schema the vault no longer declares.
    ///
    /// State the store judges across documents is not in it. Link health reads
    /// [`VaultSchema::ambiguity_ignore`], and a link's finding is re-decided
    /// by the store over stored facts, not derived again from the document's
    /// bytes, so that declaration is no term here: what a pin owes link health
    /// is the carrier below, whatever this answers.
    ///
    /// **A dormant carrier.** Its consuming layer is the host's heal after a
    /// pin, once the store re-decides link health over every link at the pin
    /// itself ([ADR 0027]). A pin discards every link-health finding whatever
    /// the schema declares, and today only the re-derivation of the document
    /// holding a link files its finding again, so the heal re-derives every
    /// row below the pin and asks nothing of this.
    ///
    /// [ADR 0027]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0027-link-health-rides-the-changeset.md
    pub fn rederives_documents(&self) -> bool {
        self.tags.reports_undeclared() || !self.fields.is_empty() || !self.rules.is_empty()
    }

    /// The type a field's declaration gives it, or text where nothing declares
    /// it.
    ///
    /// **Text is the floor rather than a refusal.** A vault that declares no
    /// fields still sorts and compares them, and it does so as the text they
    /// are written as; a declaration is what changes that answer.
    pub fn declared_type(&self, key: &str) -> FieldType {
        self.field(key).map_or(FieldType::Text, DeclaredField::kind)
    }

    /// Reads one raw frontmatter value under the type its field declares.
    ///
    /// This is the single typing function: a sort, a range predicate and a
    /// comparison operator all reach a typed value through it, so one rule
    /// decides that dates order chronologically and numbers numerically.
    pub fn typed(&self, key: &str, raw: &str) -> Result<TypedValue, typed::NotThisType> {
        self.declared_type(key).read(raw)
    }
}

/// One declared frontmatter field: its type and, where declared, its shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclaredField {
    kind: FieldType,
    shape: Option<Shape>,
}

impl DeclaredField {
    /// The field's declared type. Under [`Shape::List`] it is each element's.
    pub fn kind(&self) -> FieldType {
        self.kind
    }

    /// The field's declared shape, where the schema declares one; a field
    /// declaring none admits either.
    ///
    /// Read at schema read, where a rule's default and selectors are judged
    /// against it; by selection and rule judgment, which read a value under
    /// it — a selector on a key declared [`Shape::Single`] reads a scalar
    /// value alone, and a value of the other shape is a shape mismatch
    /// ([`VaultSchema::judge`]); and by `norn-host`, which carries it into
    /// the declaration `describe` reports it with.
    pub fn shape(&self) -> Option<Shape> {
        self.shape
    }
}

/// Whether a field holds one value or a list of them.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Shape {
    /// One value, never a list.
    Single,
    /// A list of values, each read as the field's declared type.
    List,
}

impl Shape {
    /// Every shape the grammar holds, in declaration order.
    pub const ALL: [Shape; 2] = [Shape::Single, Shape::List];

    /// The shape as the schema spells it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Shape::Single => "single",
            Shape::List => "list",
        }
    }

    /// The shape a schema's spelling names, or nothing.
    pub fn named(spelling: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|shape| shape.as_str() == spelling)
    }
}

impl fmt::Display for Shape {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// What the vault declares about its `#tag` vocabulary.
///
/// **Every comparison here reads the tag fold** ([`fold_tag`]): a declared
/// name, a pattern and the tag judged against them are each folded before
/// they are compared, so `#Work` is admitted by a declared `work` and
/// `#Area/Work` by the pattern `area/**`. The fold takes each character alone,
/// so a pattern's literal characters fold as a tag's do whatever wildcard
/// stands beside them. A name or a pattern written twice under the fold is
/// one declaration, held at the spelling the schema writes first.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TagFacet {
    /// Each declared name's first spelling, keyed by its fold.
    declared: BTreeMap<String, String>,
    /// Each pattern at its first spelling under the tag fold, in the order
    /// written.
    patterns: Vec<Pattern>,
    /// Each pattern with its text folded, in the order of `patterns`: the
    /// reading a folded tag is matched against.
    folded_patterns: Vec<Pattern>,
    undeclared: UndeclaredTags,
}

impl TagFacet {
    /// The literal tag names the vault declares, each at its first spelling,
    /// in the order of their folds.
    pub fn declared(&self) -> impl Iterator<Item = &str> {
        self.declared.values().map(String::as_str)
    }

    /// The patterns the facet admits beyond its literal names, each at its
    /// first spelling under the tag fold, in the order written.
    pub fn patterns(&self) -> &[Pattern] {
        &self.patterns
    }

    /// What the vault says about a tag it did not declare.
    pub fn undeclared(&self) -> UndeclaredTags {
        self.undeclared
    }

    /// Whether `name` is in the declared vocabulary: its fold is a declared
    /// name's fold, or matches a pattern's folded text.
    pub fn admits(&self, name: &str) -> bool {
        let folded = fold_tag(name);
        self.declared.contains_key(&folded)
            || self
                .folded_patterns
                .iter()
                .any(|pattern| pattern.matches(&folded, CaseFold::Exact))
    }

    /// Whether a tag outside the vocabulary is a finding.
    ///
    /// False for a facet that declares nothing: a vault with no declared tag
    /// vocabulary is not a vault where every tag is undeclared, it is a vault
    /// that has not taken a position.
    pub fn reports_undeclared(&self) -> bool {
        self.undeclared == UndeclaredTags::Report
            && !(self.declared.is_empty() && self.patterns.is_empty())
    }
}

/// What the vault says about a tag its facet does not admit.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UndeclaredTags {
    /// Anything may be tagged. This is the default, so a vault that lists its
    /// tags for documentation does not start reporting on every other one.
    #[default]
    Allow,
    /// A tag outside the vocabulary is a finding.
    Report,
}

impl UndeclaredTags {
    /// Every disposition this crate reads, in declaration order.
    pub const ALL: [UndeclaredTags; 2] = [UndeclaredTags::Allow, UndeclaredTags::Report];

    /// The disposition as the schema spells it.
    pub const fn as_str(self) -> &'static str {
        match self {
            UndeclaredTags::Allow => "allow",
            UndeclaredTags::Report => "report",
        }
    }
}

/// Why schema bytes are not a content model.
///
/// Every variant is a refusal of the *file*, not a judgment about a document:
/// a schema that does not read leaves the vault with no declaration at all,
/// which is a reload refusal rather than a finding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VaultSchemaError {
    /// The file is not text.
    NotUtf8 { message: String },
    /// The text is not YAML.
    NotYaml { message: String },
    /// The document's root is not a mapping.
    NotAMapping { found: &'static str },
    /// `version` is absent, is not an integer, or names a grammar this build
    /// does not read.
    Version { detail: String },
    /// A section holds something other than the shape it is declared in.
    Section {
        /// The dotted path to the offending node, `fields.created.type`.
        at: String,
        /// What the grammar wanted there.
        wanted: &'static str,
        /// What is there instead.
        found: String,
    },
    /// A key this grammar does not hold. A schema carrying one states
    /// something this build cannot act on, exactly as a later version does.
    UnknownKey {
        /// The dotted path to the section holding it, empty at the root.
        section: String,
        /// The key itself, as it is written.
        key: String,
        /// The keys the section does hold, in grammar order.
        known: &'static [&'static str],
    },
    /// A creation rule or the inbox breaks the template grammar or a rule
    /// placed on where a token stands.
    Creation {
        /// The dotted path to the offending node, `creatable.task.target`.
        at: String,
        /// What is wrong there.
        problem: CreationProblem,
    },
    /// A glob the schema states breaks a rule placed on its segments.
    Glob {
        /// The dotted path to the glob, `rules.task.match.path`.
        at: String,
        /// The glob, as written.
        glob: String,
        /// What is wrong with it.
        problem: GlobProblem,
    },
    /// A schema rule states something its own constraints, the field
    /// declarations or the template grammar refuse.
    Rule {
        /// The dotted path to the offending node,
        /// `rules.task.required.status.default`.
        at: String,
        /// What is wrong there.
        problem: RuleProblem,
    },
    /// The allowed paths of a rule and of every rule that may select a
    /// document beside it weigh more than [`PLACEMENT_CEILING`]: the bound on
    /// the walk that decides whether those rules leave a document any path.
    PlacementCeiling {
        /// The rule whose neighbourhood is weighed.
        rule: String,
        /// That rule and every rule with allowed paths that may select a
        /// document beside it, in name order.
        rules: Vec<String>,
        /// What they weigh: the product of each rule's allowed-path weight.
        weight: u64,
    },
    /// Rules that select every document any of them selects together state
    /// constraints no document can meet together, or one rule's allowed
    /// paths admit no document path.
    RulesConflict {
        /// The conflict, naming every contributing rule.
        conflict: RulesConflict,
    },
}

/// What is wrong with a glob the schema states.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GlobProblem {
    /// A rule's `exclude.path` or `allowed_paths` glob holds `<` or `>`. In
    /// a rule glob they spell a path capture, which only `match.path` binds,
    /// so one elsewhere is refused rather than read as literal text; a
    /// wildcard matches a literal `<x>` segment there: `?x?`.
    CaptureOutsideMatch,
    /// A `<NAME>` capture whose name is not an identifier.
    CaptureName {
        /// The name, as written between the brackets.
        name: String,
    },
    /// A `<` or `>` that does not spell a whole segment: a capture binds a
    /// whole path segment or nothing.
    CaptureNotWholeSegment {
        /// The segment, as written.
        segment: String,
    },
    /// One capture name written twice.
    CaptureTwice {
        /// The name written twice.
        name: String,
    },
    /// An empty segment — a leading, trailing or doubled `/` — which no
    /// document path holds.
    EmptySegment,
}

impl fmt::Display for GlobProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        const IDENTIFIER: &str = "an ASCII letter or `_` followed by letters, digits, `_` or `-`";
        match self {
            GlobProblem::CaptureOutsideMatch => formatter.write_str(
                "holds `<` or `>`, which spell a path capture in a rule glob, and only a rule's `match.path` binds one; a wildcard such as `?x?` matches a literal `<x>`",
            ),
            GlobProblem::CaptureName { name } => write!(
                formatter,
                "captures `<{name}>`, and a capture's name is {IDENTIFIER}"
            ),
            GlobProblem::CaptureNotWholeSegment { segment } => write!(
                formatter,
                "holds the segment `{segment}`, and a capture `<NAME>` is a whole segment"
            ),
            GlobProblem::CaptureTwice { name } => {
                write!(formatter, "captures `<{name}>` twice")
            }
            GlobProblem::EmptySegment => formatter.write_str(
                "holds an empty segment, which no document path holds",
            ),
        }
    }
}

impl fmt::Display for VaultSchemaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VaultSchemaError::NotUtf8 { message } => {
                write!(formatter, "the vault schema is not UTF-8: {message}")
            }
            VaultSchemaError::NotYaml { message } => {
                write!(formatter, "the vault schema is not YAML: {message}")
            }
            VaultSchemaError::NotAMapping { found } => {
                write!(
                    formatter,
                    "the vault schema is {found}, and it must be a mapping"
                )
            }
            VaultSchemaError::Version { detail } => write!(formatter, "{detail}"),
            VaultSchemaError::Section { at, wanted, found } => {
                write!(formatter, "`{at}` is {found}, and it must be {wanted}")
            }
            VaultSchemaError::UnknownKey {
                section,
                key,
                known,
            } => write!(
                formatter,
                "`{}` is not a key the vault schema grammar holds; {} holds {}",
                dotted(section, key),
                if section.is_empty() {
                    "the schema"
                } else {
                    section.as_str()
                },
                known.join(", ")
            ),
            VaultSchemaError::Creation { at, problem } => write!(formatter, "`{at}` {problem}"),
            VaultSchemaError::Glob { at, glob, problem } => {
                write!(formatter, "`{at}` holds the glob `{glob}`, which {problem}")
            }
            VaultSchemaError::Rule { at, problem } => write!(formatter, "`{at}` {problem}"),
            VaultSchemaError::PlacementCeiling {
                rule,
                rules,
                weight,
            } => write!(
                formatter,
                "the allowed paths of the rule `{rule}` and of every rule that may select a document beside it ({}) weigh {weight}, past the ceiling of {PLACEMENT_CEILING}",
                rules::named(rules)
            ),
            VaultSchemaError::RulesConflict { conflict } => write!(
                formatter,
                "rules that select every document any of them selects conflict: {conflict}"
            ),
        }
    }
}

/// One node's dotted path, which at the root is the key alone.
fn dotted(section: &str, key: &str) -> String {
    if section.is_empty() {
        key.to_string()
    } else {
        format!("{section}.{key}")
    }
}

impl std::error::Error for VaultSchemaError {}

/// The name this grammar calls a YAML node's shape, for a refusal to state.
fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Sequence(_) => "a sequence",
        Value::Mapping(_) => "a mapping",
        Value::Tagged(_) => "a tagged value",
    }
}

fn section_error(at: &str, wanted: &'static str, found: &Value) -> VaultSchemaError {
    VaultSchemaError::Section {
        at: at.to_string(),
        wanted,
        found: type_name(found).to_string(),
    }
}

/// Refuse every key of `mapping` the grammar does not hold at `section`.
///
/// **Unknown is refused rather than ignored.** A key nothing reads is a
/// declaration the author believes is in force, and the ones this grammar is
/// most likely to meet — a misspelled section, a misspelled disposition — turn
/// a vault's reporting posture off in silence. `section` is the dotted path to
/// the mapping, empty at the root.
fn known_keys_only(
    section: &str,
    mapping: &serde_yaml::Mapping,
    known: &'static [&'static str],
) -> Result<(), VaultSchemaError> {
    for key in mapping.keys() {
        let Some(key) = key.as_str() else {
            return Err(section_error(section, "a mapping keyed by name", key));
        };
        if !known.contains(&key) {
            return Err(VaultSchemaError::UnknownKey {
                section: section.to_string(),
                key: key.to_string(),
                known,
            });
        }
    }
    Ok(())
}

fn at<'a>(document: &'a serde_yaml::Mapping, key: &str) -> Option<&'a Value> {
    document
        .get(Value::String(key.to_string()))
        .filter(|value| !value.is_null())
}

/// The version schema `text` states, read as [`VaultSchema::parse`] reads it
/// and judged against no version: what the migration ladder walks from.
///
/// **A schema that holds no document states the version this build reads**,
/// since it is the declaration that declares nothing, which every version
/// spells alike; a document that is not a mapping, or states no integer
/// `version`, states none.
pub(crate) fn stated_version(text: &str) -> Result<i64, VaultSchemaError> {
    let document: Value =
        serde_yaml::from_str(text).map_err(|error| VaultSchemaError::NotYaml {
            message: error.to_string(),
        })?;
    match document {
        Value::Null => Ok(SCHEMA_VERSION),
        Value::Mapping(mapping) => version_in(&mapping),
        other => Err(VaultSchemaError::NotAMapping {
            found: type_name(&other),
        }),
    }
}

/// The integer `version` `document` states.
fn version_in(document: &serde_yaml::Mapping) -> Result<i64, VaultSchemaError> {
    let Some(value) = at(document, "version") else {
        return Err(VaultSchemaError::Version {
            detail: "the vault schema carries no `version`, so its grammar is unknown".to_string(),
        });
    };
    value.as_i64().ok_or_else(|| VaultSchemaError::Version {
        detail: format!(
            "`version` is {}, and a schema version is an integer",
            type_name(value)
        ),
    })
}

fn read_version(document: &serde_yaml::Mapping) -> Result<(), VaultSchemaError> {
    let found = version_in(document)?;
    if found != SCHEMA_VERSION {
        return Err(VaultSchemaError::Version {
            detail: format!(
                "the vault schema is at version {found} and this build reads {SCHEMA_VERSION}"
            ),
        });
    }
    Ok(())
}

fn read_fields(
    document: &serde_yaml::Mapping,
) -> Result<BTreeMap<String, DeclaredField>, VaultSchemaError> {
    let Some(value) = at(document, "fields") else {
        return Ok(BTreeMap::new());
    };
    let Value::Mapping(fields) = value else {
        return Err(section_error(
            "fields",
            "a mapping of field name to declaration",
            value,
        ));
    };
    fields
        .iter()
        .map(|(key, declaration)| {
            let key = key
                .as_str()
                .ok_or_else(|| section_error("fields", "a mapping keyed by field name", key))?;
            Ok((key.to_string(), read_field(key, declaration)?))
        })
        .collect()
}

fn read_field(key: &str, declaration: &Value) -> Result<DeclaredField, VaultSchemaError> {
    let Value::Mapping(declaration) = declaration else {
        return Err(section_error(
            &format!("fields.{key}"),
            "a mapping",
            declaration,
        ));
    };
    known_keys_only(&format!("fields.{key}"), declaration, FIELD_KEYS)?;
    let kind = match at(declaration, "type") {
        None => FieldType::Text,
        Some(value) => value.as_str().and_then(FieldType::named).ok_or_else(|| {
            section_error(&format!("fields.{key}.type"), "a declared type", value)
        })?,
    };
    let shape = match at(declaration, "shape") {
        None => None,
        Some(value) => Some(value.as_str().and_then(Shape::named).ok_or_else(|| {
            section_error(&format!("fields.{key}.shape"), "`single` or `list`", value)
        })?),
    };
    Ok(DeclaredField { kind, shape })
}

fn read_tags(document: &serde_yaml::Mapping) -> Result<TagFacet, VaultSchemaError> {
    let Some(value) = at(document, "tags") else {
        return Ok(TagFacet::default());
    };
    let Value::Mapping(tags) = value else {
        return Err(section_error("tags", "a mapping", value));
    };
    known_keys_only("tags", tags, TAG_KEYS)?;
    let declared = match at(tags, "declared") {
        None => BTreeMap::new(),
        Some(value) => read_tag_names("tags.declared", value)?,
    };
    let (patterns, folded_patterns) = match at(tags, "patterns") {
        None => (Vec::new(), Vec::new()),
        Some(value) => fold_tag_patterns(read_patterns("tags.patterns", value)?),
    };
    let undeclared = match at(tags, "undeclared") {
        None => UndeclaredTags::default(),
        Some(value) => match value.as_str() {
            Some("allow") => UndeclaredTags::Allow,
            Some("report") => UndeclaredTags::Report,
            _ => {
                return Err(section_error(
                    "tags.undeclared",
                    "`allow` or `report`",
                    value,
                ));
            }
        },
    };
    Ok(TagFacet {
        declared,
        patterns,
        folded_patterns,
        undeclared,
    })
}

fn read_ambiguity_ignore(document: &serde_yaml::Mapping) -> Result<Vec<Pattern>, VaultSchemaError> {
    let Some(value) = at(document, "paths") else {
        return Ok(Vec::new());
    };
    let Value::Mapping(paths) = value else {
        return Err(section_error("paths", "a mapping", value));
    };
    known_keys_only("paths", paths, PATH_KEYS)?;
    match at(paths, "ambiguity_ignore") {
        None => Ok(Vec::new()),
        Some(value) => read_patterns("paths.ambiguity_ignore", value),
    }
}

/// A sequence of tag names, each held at its first spelling under its fold.
fn read_tag_names(
    at_path: &str,
    value: &Value,
) -> Result<BTreeMap<String, String>, VaultSchemaError> {
    let Value::Sequence(items) = value else {
        return Err(section_error(at_path, "a sequence of strings", value));
    };
    let mut names = BTreeMap::new();
    for item in items {
        let name = item
            .as_str()
            .ok_or_else(|| section_error(at_path, "a sequence of strings", item))?;
        names
            .entry(fold_tag(name))
            .or_insert_with(|| name.to_string());
    }
    Ok(names)
}

/// Tag patterns held once under the tag fold, each at its first spelling, in
/// the order written: the patterns as written, and beside each its text
/// folded, which is what a folded tag is matched against.
fn fold_tag_patterns(patterns: Vec<Pattern>) -> (Vec<Pattern>, Vec<Pattern>) {
    let mut seen = BTreeSet::new();
    patterns
        .into_iter()
        .filter_map(|pattern| {
            let folded = fold_tag(pattern.as_str());
            seen.insert(folded.clone()).then(|| {
                let folded = Pattern::parse(&folded)
                    .expect("a pattern's fold is as long as the pattern, so it is not empty");
                (pattern, folded)
            })
        })
        .unzip()
}

fn read_patterns(at_path: &str, value: &Value) -> Result<Vec<Pattern>, VaultSchemaError> {
    let Value::Sequence(items) = value else {
        return Err(section_error(at_path, "a sequence of patterns", value));
    };
    items
        .iter()
        .map(|item| {
            let source = item
                .as_str()
                .ok_or_else(|| section_error(at_path, "a sequence of patterns", item))?;
            Pattern::parse(source).map_err(|error| VaultSchemaError::Section {
                at: at_path.to_string(),
                wanted: "a pattern",
                found: error.to_string(),
            })
        })
        .collect()
}
