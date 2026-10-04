//! The vault schema's content model — what a vault declares about itself.
//!
//! A vault schema is a YAML file the author writes and norn never edits. Until
//! this module existed it was bytes: read, hashed, pinned, and opaque to every
//! consumer. [`VaultSchema`] is the typed reading of those bytes — the declared
//! fields with their types and rules, the declared tag facet, the declared
//! folders, the path rules, and the creation rules and inbox that say how a
//! new document is made — and it is what makes a declaration something
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
//!     required: true
//!   created:
//!     type: date
//!   status:
//!     type: text
//!     one_of: [draft, live, retired]
//! tags:
//!   declared: [project, area]
//!   patterns: ["person/**"]
//!   undeclared: report
//! folders:
//!   - path: journal
//!     description: One document per day
//! paths:
//!   ambiguity_ignore: ["archive/**"]
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
//! [`VaultSchema::rederives_documents`] is how a caller asks whether it is
//! worth re-deriving any document under it.
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
//! either.

pub mod creation;
pub mod template;
pub mod typed;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_yaml::Value;

pub use creation::{CreationProblem, CreationRule, Inbox, SeqSlot, Target};
use norn_wire::fold_tag;
pub use norn_wire::{CaseFold, Pattern, PatternError};
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
    "folders",
    "paths",
    "creatable",
    "inbox",
];

/// The keys one field's declaration holds.
const FIELD_KEYS: &[&str] = &["type", "required", "one_of"];

/// The keys the tag facet holds.
const TAG_KEYS: &[&str] = &["declared", "patterns", "undeclared"];

/// The keys one folder's declaration holds.
const FOLDER_KEYS: &[&str] = &["path", "description"];

/// The keys the path rules hold.
const PATH_KEYS: &[&str] = &["ambiguity_ignore"];

/// One vault's declaration about itself.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VaultSchema {
    fields: BTreeMap<String, DeclaredField>,
    tags: TagFacet,
    folders: Vec<DeclaredFolder>,
    ambiguity_ignore: Vec<Pattern>,
    creation_rules: BTreeMap<String, CreationRule>,
    inbox: Option<Inbox>,
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
        Ok(VaultSchema {
            fields: read_fields(&document)?,
            tags: read_tags(&document)?,
            folders: read_folders(&document)?,
            ambiguity_ignore: read_ambiguity_ignore(&document)?,
            creation_rules: creation::read_creatable(&document)?,
            inbox: creation::read_inbox(&document)?,
        })
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

    /// The declared folders, in the order they were written, each path once
    /// and without its trailing `/`.
    ///
    /// Read by derivation, which hands them to the store with the rest of the
    /// declaration, and `describe` reports each as a facet. No derivation
    /// judges a document by the folder it stands in.
    pub fn folders(&self) -> &[DeclaredFolder] {
        &self.folders
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
    /// The re-derivation a schema change implies costs the vault, so the
    /// question is asked before it is paid. The answer is the disjunction over
    /// the declarations some per-document derived state reads — state a
    /// document's own re-derivation derives again — and that set holds two:
    ///
    /// - **A tag facet that reports**, whose findings are derived per document.
    /// - **A field declared with a type that does not order as text**, whose
    ///   values the field pillar's typed column holds. A pin clears that
    ///   column, so a schema declaring one owes every document standing under
    ///   it the re-derivation that refills it, whether or not its bytes moved.
    ///   A field declared as text or tags orders as its raw text and fills
    ///   nothing.
    ///
    /// Creation rules and the inbox are no term: they say how a document is
    /// made, and no row a document's derivation writes reads them.
    ///
    /// A schema declaring neither leaves every row with the same per-document
    /// derived state under the new pin as under the old. **A declaration
    /// gaining a per-document consumer joins this disjunction in the same
    /// change**: a schema answering `false` here while some per-document state
    /// reads its declaration would leave that state derived under a schema the
    /// vault no longer declares.
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
        self.tags.reports_undeclared()
            || self
                .fields
                .values()
                .any(|field| !field.kind().orders_as_text())
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

/// One declared frontmatter field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclaredField {
    kind: FieldType,
    required: bool,
    one_of: Option<BTreeSet<String>>,
}

impl DeclaredField {
    /// The field's declared type.
    pub fn kind(&self) -> FieldType {
        self.kind
    }

    /// Whether every document is declared to carry this field.
    ///
    /// Read by `describe`, which reports it with the field's declaration, and
    /// by the field-rule finding kinds, which are not built: a missing required
    /// field is a finding a derivation mints under the schema fingerprint the
    /// way the tag facet's is. The current call graph does not reach that
    /// consumer, because the only finding kind a schema keys today is the tag
    /// facet's.
    pub fn required(&self) -> bool {
        self.required
    }

    /// The closed set of values the field is declared to hold, where it is
    /// declared closed.
    ///
    /// Read by the same two consumers as [`DeclaredField::required`]:
    /// `describe`, which reports the closed set as part of the declaration,
    /// and the finding a value outside it mints, which is not built.
    pub fn one_of(&self) -> Option<impl Iterator<Item = &str>> {
        self.one_of
            .as_ref()
            .map(|values| values.iter().map(String::as_str))
    }
}

/// One declared folder.
///
/// A name and what it is for. What a folder *requires* of the documents inside
/// it is a rule family with its own invalidation key — a document's path — and
/// the declaration arrives with the derivation that reads it.
///
/// **A folder-scoped rule reopens a carried move's schema check.** The host's
/// applier does not judge a document a move carries byte for byte again,
/// because no rule yet concludes about a document from its folder; one that
/// did would make that skip unsound (`norn-host`'s `applier::stage`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclaredFolder {
    path: String,
    description: Option<String>,
}

impl DeclaredFolder {
    /// The vault-root-relative path the folder is at.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// What the schema says the folder is for.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
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
    /// `folders` declares one folder path twice. A trailing `/` does not make
    /// a second folder, so `journal` and `journal/` are one path.
    RepeatedFolder {
        /// The path declared twice, without its trailing `/`.
        path: String,
    },
    /// A creation rule or the inbox breaks the template grammar or a rule
    /// placed on where a token stands.
    Creation {
        /// The dotted path to the offending node, `creatable.task.target`.
        at: String,
        /// What is wrong there.
        problem: CreationProblem,
    },
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
            VaultSchemaError::RepeatedFolder { path } => {
                write!(formatter, "`folders` declares the folder `{path}` twice")
            }
            VaultSchemaError::Creation { at, problem } => write!(formatter, "`{at}` {problem}"),
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
    let required = match at(declaration, "required") {
        None => false,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| section_error(&format!("fields.{key}.required"), "a boolean", value))?,
    };
    let one_of = match at(declaration, "one_of") {
        None => None,
        Some(value) => Some(read_strings(&format!("fields.{key}.one_of"), value)?),
    };
    Ok(DeclaredField {
        kind,
        required,
        one_of,
    })
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

fn read_folders(document: &serde_yaml::Mapping) -> Result<Vec<DeclaredFolder>, VaultSchemaError> {
    let Some(value) = at(document, "folders") else {
        return Ok(Vec::new());
    };
    let Value::Sequence(folders) = value else {
        return Err(section_error("folders", "a sequence", value));
    };
    let mut declared = BTreeSet::new();
    folders
        .iter()
        .map(|folder| {
            let Value::Mapping(folder) = folder else {
                return Err(section_error("folders", "a sequence of mappings", folder));
            };
            known_keys_only("folders", folder, FOLDER_KEYS)?;
            // Absent and present-but-wrong-shape are two refusals. A folder
            // that never wrote `path` is missing a declaration; a folder that
            // wrote `path: 2026` holds one the grammar cannot read, and the
            // author is told which of the two they wrote.
            let Some(path) = at(folder, "path") else {
                return Err(VaultSchemaError::Section {
                    at: "folders.path".to_string(),
                    wanted: "a path",
                    found: "absent".to_string(),
                });
            };
            // A folder path is read without its trailing `/`: `journal/` is
            // the folder `journal`.
            let path = path
                .as_str()
                .ok_or_else(|| section_error("folders.path", "a path", path))?
                .trim_end_matches('/');
            // A path declared twice is refused as a repeated key is, rather
            // than leaving which declaration stands to the order they were
            // written in.
            if !declared.insert(path) {
                return Err(VaultSchemaError::RepeatedFolder {
                    path: path.to_string(),
                });
            }
            let description = match at(folder, "description") {
                None => None,
                Some(value) => Some(
                    value
                        .as_str()
                        .ok_or_else(|| section_error("folders.description", "a string", value))?
                        .to_string(),
                ),
            };
            Ok(DeclaredFolder {
                path: path.to_string(),
                description,
            })
        })
        .collect()
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

fn read_strings(at_path: &str, value: &Value) -> Result<BTreeSet<String>, VaultSchemaError> {
    let Value::Sequence(items) = value else {
        return Err(section_error(at_path, "a sequence of strings", value));
    };
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| section_error(at_path, "a sequence of strings", item))
        })
        .collect()
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
