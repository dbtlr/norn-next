//! The vault schema's content model — what a vault declares about itself.
//!
//! A vault schema is a YAML file the author writes and norn never edits. Until
//! this module existed it was bytes: read, hashed, pinned, and opaque to every
//! consumer. [`VaultSchema`] is the typed reading of those bytes — the declared
//! fields with their types and rules, the declared tag facet, the declared
//! folders, and the path rules — and it is what makes a declaration something
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
//! ```
//!
//! Every section is optional. A schema that declares nothing — which is what
//! `version: 1` alone is — is a valid schema that judges no document, and
//! [`VaultSchema::rederives_documents`] is how a caller asks whether it is
//! worth re-deriving anything under it.
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
//! same set as path rules.

pub mod typed;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_yaml::Value;

pub use norn_wire::{CaseFold, Pattern, PatternError};
pub use typed::{Comparison, ComparisonSignal, FieldType, TypedValue};

/// The schema version this build reads.
///
/// A schema that states another version is refused rather than read under
/// this one's meanings: a declaration written for a grammar this build does
/// not have is not a declaration this build can honour.
pub const SCHEMA_VERSION: i64 = 1;

/// The sections a schema declares, which is every key its root holds.
const ROOT_KEYS: &[&str] = &["version", "fields", "tags", "folders", "paths"];

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
    /// a target opens, which a find's `resolves` part reads today, and
    /// `describe` reports each glob of it as a path rule. The globs match under
    /// the store's recorded path order: with ASCII case folded on a root that
    /// folds it, bytewise on a root that does not.
    ///
    /// Backlinks and link-health findings are the dormant consumers of the
    /// same exclusion: the link index lands them in Layer 3, and they read a
    /// link target's class through that one resolver. The current call graph
    /// does not reach them, because no link index exists yet.
    pub fn ambiguity_ignore(&self) -> &[Pattern] {
        &self.ambiguity_ignore
    }

    /// Whether a pin of this schema obliges a re-derivation of the documents
    /// standing under it.
    ///
    /// The re-derivation a schema change implies costs the vault, so the
    /// question is asked before it is paid. The answer is the disjunction over
    /// the declarations some derived state reads, and that set holds two:
    ///
    /// - **A tag facet that reports**, whose findings are derived per document.
    /// - **A field declared with a type that does not order as text**, whose
    ///   values the field pillar's typed column holds. A pin clears that
    ///   column, so a schema declaring one owes every document standing under
    ///   it the re-derivation that refills it, whether or not its bytes moved.
    ///   A field declared as text or tags orders as its raw text and fills
    ///   nothing.
    ///
    /// A schema declaring neither leaves every row with the same derived state
    /// under the new pin as under the old. **A declaration gaining a consumer
    /// joins this disjunction in the same change**: a schema answering `false`
    /// here while some derived state reads its declaration would leave that
    /// state derived under a schema the vault no longer declares.
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
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TagFacet {
    declared: BTreeSet<String>,
    patterns: Vec<Pattern>,
    undeclared: UndeclaredTags,
}

impl TagFacet {
    /// The literal tag names the vault declares, in name order.
    pub fn declared(&self) -> impl Iterator<Item = &str> {
        self.declared.iter().map(String::as_str)
    }

    /// The patterns the facet admits beyond its literal names.
    pub fn patterns(&self) -> &[Pattern] {
        &self.patterns
    }

    /// What the vault says about a tag it did not declare.
    pub fn undeclared(&self) -> UndeclaredTags {
        self.undeclared
    }

    /// Whether `name` is in the declared vocabulary.
    ///
    /// Case is compared as written, because deciding that `#Work` and `#work`
    /// are one tag is a matching policy the syntax layer deliberately leaves
    /// open and a schema that wants both declares both.
    pub fn admits(&self, name: &str) -> bool {
        self.declared.contains(name)
            || self
                .patterns
                .iter()
                .any(|pattern| pattern.matches(name, CaseFold::Exact))
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

fn read_version(document: &serde_yaml::Mapping) -> Result<(), VaultSchemaError> {
    let Some(value) = at(document, "version") else {
        return Err(VaultSchemaError::Version {
            detail: "the vault schema carries no `version`, so its grammar is unknown".to_string(),
        });
    };
    let Some(found) = value.as_i64() else {
        return Err(VaultSchemaError::Version {
            detail: format!(
                "`version` is {}, and a schema version is an integer",
                type_name(value)
            ),
        });
    };
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
        None => BTreeSet::new(),
        Some(value) => read_strings("tags.declared", value)?,
    };
    let patterns = match at(tags, "patterns") {
        None => Vec::new(),
        Some(value) => read_patterns("tags.patterns", value)?,
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
