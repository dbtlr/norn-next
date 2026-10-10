//! Creation rules and the inbox: how the schema says a new document is made.
//!
//! A **creation rule** is named, and names a templated target path, the
//! variables a caller must supply, frontmatter defaults and a body. The
//! **inbox** is the one place untyped capture lands, and names a target alone.
//! Both are written in the grammar of [`Template`].
//!
//! **Everything about where a token may stand is judged at schema read.** A
//! target ends in `.md`, is relative, stays inside the vault, and is a document
//! path by the one grammar [`PathProblem::of_document`] writes — no leading
//! `/`, no empty, `.` or `..` segment, no `\`, no control character, and no
//! file name that is `.` or `..` once its extension is dropped — judged on its
//! literal text and where its tokens stand, since a
//! token's value is not known until a document is made. A target holds no `:`,
//! which is not portable in a file name, so `{{now}}` and `{{time}}` stand in
//! one only as `{{now|slug}}` and `{{time|slug}}`. `{{seq}}` stands only in a
//! target, at most once, and only in its file name. A `{{var.NAME}}` anywhere
//! in a rule names a variable the rule declares. A frontmatter default's key
//! is the field name it lands as: not empty, not the merge key `<<`, and
//! holding no `{{`. The inbox's target carries `{{seq}}` and names no
//! variable, since untyped capture is supplied nothing. No template of a
//! creation rule or the inbox reads a path capture: `{{path.NAME}}` reads what
//! a schema rule's `match.path` bound, and a creation rule matches no path.
//!
//! **What a token's value may be is judged when the rule is filled.** A value
//! filled into a target that is empty, holds `/`, `\` or `:`, or is `.` or
//! `..` is refused, after its filter, and the whole path the target fills to
//! is judged by the same document-path rules its literal text was, so a
//! target fills to a path inside the vault that the store can hold, whatever
//! it is supplied. A body and a frontmatter default hold
//! any value. Where a target is numbered, [`Target::seq_slot`] names the
//! folder and the file name around the number, which is what allocating a
//! number reads.
//!
//! **A rule derives nothing.** No row a document's derivation writes reads a
//! creation rule or the inbox, so neither is a term of
//! [`VaultSchema::rederives_documents`](super::VaultSchema::rederives_documents).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_yaml::{Mapping, Value};

use norn_wire::{AuthoredValue, DocumentPath, FiniteFloat, PathProblem, ValueMap};

use super::template::{
    FillError, Part, Slot, Template, TemplateError, TemplateValues, Token, UnsafeValue,
    is_identifier,
};
use super::{VaultSchemaError, at, known_keys_only, section_error};

/// The keys one creation rule holds.
const RULE_KEYS: &[&str] = &["target", "variables", "frontmatter_defaults", "body"];

/// The keys the inbox holds.
const INBOX_KEYS: &[&str] = &["target"];

/// One named way to make a document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreationRule {
    name: String,
    target: Target,
    variables: Vec<String>,
    frontmatter_defaults: Vec<(String, DefaultValue)>,
    body: Option<Template>,
}

impl CreationRule {
    /// The name the rule is asked for by: `new --as task`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Where a document the rule makes is written.
    pub fn target(&self) -> &Target {
        &self.target
    }

    /// The variables a caller must supply, in the order the schema writes
    /// them, each once.
    pub fn variables(&self) -> &[String] {
        &self.variables
    }

    /// The frontmatter a document the rule makes starts with, as the schema
    /// writes it: every string scalar is a template's source text, and every
    /// map keeps the order its keys are written in.
    pub fn frontmatter_defaults(&self) -> ValueMap {
        source_map(&self.frontmatter_defaults)
    }

    /// The body a document the rule makes starts with, where the rule writes
    /// one.
    pub fn body(&self) -> Option<&Template> {
        self.body.as_ref()
    }

    /// The frontmatter a document the rule makes starts with, filled under
    /// `values`: every string scalar, however deep, filled as a template,
    /// and every other value, every key and every order kept as written.
    pub fn fill_frontmatter_defaults(
        &self,
        values: &TemplateValues,
    ) -> Result<ValueMap, FillError> {
        fill_map(&self.frontmatter_defaults, values)
    }
}

/// Where untyped capture lands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Inbox {
    target: Target,
}

impl Inbox {
    /// Where a captured document is written. It carries `{{seq}}` and names no
    /// variable.
    pub fn target(&self) -> &Target {
        &self.target
    }
}

/// A target path template that ends in `.md`, stays inside the vault, and
/// holds `{{seq}}` at most once and only in its file name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Target {
    template: Template,
}

impl Target {
    /// The target as it is written.
    pub fn as_str(&self) -> &str {
        self.template.as_str()
    }

    /// The template the target is.
    pub fn template(&self) -> &Template {
        &self.template
    }

    /// Whether the target numbers its documents with `{{seq}}`.
    pub fn has_seq(&self) -> bool {
        self.template.seq_count() > 0
    }

    /// `template` as a target, or the reason it is none.
    fn read(template: Template) -> Result<Self, CreationProblem> {
        let standing = standing_in(&template);
        if let Some(problem) = PathProblem::of_document(&standing) {
            return Err(CreationProblem::Path(problem));
        }
        if standing.contains(':') {
            return Err(CreationProblem::Colon);
        }
        if let Some(token) = template.parts().iter().find_map(|part| match part {
            Part::Token(Token {
                slot: Slot::Now,
                slug: false,
            }) => Some("now"),
            Part::Token(Token {
                slot: Slot::Time,
                slug: false,
            }) => Some("time"),
            _ => None,
        }) {
            return Err(CreationProblem::ClockWithColon {
                token: token.to_string(),
            });
        }
        let segments = segments(&template);
        let file_name = segments.last().expect("splitting yields a segment");
        match file_name.last() {
            Some(Piece::Literal(text)) if text.ends_with(".md") => {}
            _ => return Err(CreationProblem::NotMarkdown),
        }
        if literal(file_name).as_deref() == Some(".md") {
            return Err(CreationProblem::FileNameEmpty);
        }
        if template.seq_count() > 1 {
            return Err(CreationProblem::SeqTwice);
        }
        let folders = &segments[..segments.len() - 1];
        if folders
            .iter()
            .flatten()
            .any(|piece| matches!(piece, Piece::Token(token) if token.slot == Slot::Seq))
        {
            return Err(CreationProblem::SeqOutsideFileName);
        }
        Ok(Target { template })
    }

    /// The vault-relative path the target fills to under `values`.
    ///
    /// Every token's value is judged after its filter, and one that is
    /// empty, holds `/`, `\` or `:`, or is `.` or `..` is refused. The whole
    /// path it fills to is then judged as a document path, by the rules schema
    /// read judged the target's literal text by, so the path is one the store
    /// can hold, inside the vault and ending in `.md`, whatever it is
    /// supplied. A numbered target fills to
    /// its [slot](Target::seq_slot) at the number `values` carries.
    pub fn fill(&self, values: &TemplateValues) -> Result<DocumentPath, FillError> {
        let path = match self.seq_slot(values)? {
            Some(slot) => slot.path(values.seq().ok_or(FillError::NoSeq)?),
            None => fill_path(self.template.parts(), values)?,
        };
        match PathProblem::of_document(&path) {
            Some(problem) => Err(FillError::NotADocumentPath { path, problem }),
            None => Ok(DocumentPath::new(path).expect("the document-path grammar admits it")),
        }
    }

    /// Where a numbered target's numbers go under `values`: the folder, and
    /// the file name's text before and after `{{seq}}`, every other token
    /// filled and judged as [`Target::fill`] judges it. `None` for a target
    /// with no `{{seq}}`. The sequence number `values` carries is not read.
    pub fn seq_slot(&self, values: &TemplateValues) -> Result<Option<SeqSlot>, FillError> {
        let parts = self.template.parts();
        let Some(at) = parts
            .iter()
            .position(|part| matches!(part, Part::Token(token) if token.slot == Slot::Seq))
        else {
            return Ok(None);
        };
        let before = fill_path(&parts[..at], values)?;
        let suffix = fill_path(&parts[at + 1..], values)?;
        // `{{seq}}` stands in the file name, so every `/` is before it.
        let (folder, prefix) = match before.rsplit_once('/') {
            Some((folder, prefix)) => (folder.to_string(), prefix.to_string()),
            None => (String::new(), before),
        };
        let slot = SeqSlot {
            folder,
            prefix,
            suffix,
        };
        // A number adds digits alone to the file name, so the slot holds a
        // document path at every number exactly where it holds one at the
        // stand-in.
        match PathProblem::of_document(&slot.spelled(STAND_IN)) {
            Some(problem) => Err(FillError::NotADocumentPath {
                path: slot.spelled("{{seq}}"),
                problem,
            }),
            None => Ok(Some(slot)),
        }
    }
}

/// Where a numbered target's numbers go, with every other value filled.
///
/// A document numbered `n` stands at [`SeqSlot::path`]`(n)`: in
/// [`SeqSlot::folder`], named [`SeqSlot::prefix`], `n` as a plain integer,
/// then [`SeqSlot::suffix`]. The folder and the text around the number are
/// what the documents already numbered in it share.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SeqSlot {
    folder: String,
    prefix: String,
    suffix: String,
}

impl SeqSlot {
    /// The vault-relative folder the numbered documents stand in, without a
    /// trailing `/`; empty at the vault root.
    pub fn folder(&self) -> &str {
        &self.folder
    }

    /// The file name's text before the number.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// The file name's text after the number, ending in `.md`.
    pub fn suffix(&self) -> &str {
        &self.suffix
    }

    /// The vault-relative path of the document numbered `seq`.
    pub fn path(&self, seq: u64) -> String {
        self.spelled(&seq.to_string())
    }

    /// The vault-relative path with `number` standing where the number does.
    fn spelled(&self, number: &str) -> String {
        let file_name = format!("{}{number}{}", self.prefix, self.suffix);
        if self.folder.is_empty() {
            file_name
        } else {
            format!("{}/{file_name}", self.folder)
        }
    }
}

/// `parts` of a target or a route filled under `values`, each token's value
/// judged.
///
/// A value is refused where it is empty after its filter, holds `/`, `\` or
/// `:`, or is `.` or `..`, so a value names no folder the target does not and
/// writes no `:`. That is not enough to hold the whole path: a value stands
/// beside literal text, and `.{{var.a}}` filled with `.` is the segment `..`.
/// So the caller judges the whole filled path as a document path too, by the
/// same rules schema read judged the target's literal text by; that check is
/// what makes a path outside the vault, or one the store cannot hold,
/// unfillable by construction, and the value rules are what name the token at
/// fault in the common case.
pub(super) fn fill_path(parts: &[Part], values: &TemplateValues) -> Result<String, FillError> {
    parts.iter().try_fold(String::new(), |mut path, part| {
        match part {
            Part::Literal(literal) => path.push_str(literal),
            Part::Token(token) => path.push_str(&path_safe(token, token.fill(values)?)?),
        }
        Ok(path)
    })
}

/// `value`, which `token` filled to, where it keeps a target's path whole.
fn path_safe(token: &Token, value: String) -> Result<String, FillError> {
    let problem = if value.is_empty() {
        UnsafeValue::Empty
    } else if value.contains(['/', '\\']) {
        UnsafeValue::Separator
    } else if value.contains(':') {
        UnsafeValue::Colon
    } else if value == "." || value == ".." {
        UnsafeValue::DotSegment
    } else {
        return Ok(value);
    };
    Err(FillError::UnsafeValue {
        token: token.to_string(),
        value,
        problem,
    })
}

/// What a token stands as when a target's literal text is judged: a value
/// that is not empty and holds no `/`, `\`, `:`, `.` or control character,
/// so the problems the stand-in has are ones every fill of the target has.
const STAND_IN: &str = "0";

/// The target's text with every token standing as [`STAND_IN`].
fn standing_in(template: &Template) -> String {
    template
        .parts()
        .iter()
        .map(|part| match part {
            Part::Literal(text) => text.as_str(),
            Part::Token(_) => STAND_IN,
        })
        .collect()
}

/// One run of a target's path segment: literal text holding no `/`, or a
/// token.
#[derive(Clone, Copy, Debug)]
enum Piece<'a> {
    Literal(&'a str),
    Token(&'a Token),
}

/// The template's path segments, split at every `/` of its literal text. A
/// segment holding no piece is empty.
fn segments(template: &Template) -> Vec<Vec<Piece<'_>>> {
    let mut segments = vec![Vec::new()];
    for part in template.parts() {
        match part {
            Part::Token(token) => segments
                .last_mut()
                .expect("there is always a segment")
                .push(Piece::Token(token)),
            Part::Literal(text) => {
                for (index, run) in text.split('/').enumerate() {
                    if index > 0 {
                        segments.push(Vec::new());
                    }
                    if !run.is_empty() {
                        segments
                            .last_mut()
                            .expect("there is always a segment")
                            .push(Piece::Literal(run));
                    }
                }
            }
        }
    }
    segments
}

/// The segment's text, where it holds no token.
fn literal(segment: &[Piece<'_>]) -> Option<String> {
    segment
        .iter()
        .map(|piece| match piece {
            Piece::Literal(text) => Some(*text),
            Piece::Token(_) => None,
        })
        .collect()
}

/// One frontmatter default: a value written as is, or a string scalar, which
/// is a template.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DefaultValue {
    /// Null, a boolean or a number.
    Plain(AuthoredValue),
    Text(Template),
    List(Vec<DefaultValue>),
    Map(Vec<(String, DefaultValue)>),
}

impl DefaultValue {
    /// The value as the schema writes it.
    pub(crate) fn source(&self) -> AuthoredValue {
        match self {
            DefaultValue::Plain(value) => value.clone(),
            DefaultValue::Text(template) => AuthoredValue::string(template.as_str()),
            DefaultValue::List(items) => AuthoredValue::list(items.iter().map(Self::source)),
            DefaultValue::Map(entries) => AuthoredValue::Map(source_map(entries)),
        }
    }

    /// The value filled under `values`: a string as its template fills, and
    /// every other value as written.
    pub(crate) fn fill(&self, values: &TemplateValues) -> Result<AuthoredValue, FillError> {
        Ok(match self {
            DefaultValue::Plain(value) => value.clone(),
            DefaultValue::Text(template) => AuthoredValue::String(template.fill(values)?),
            DefaultValue::List(items) => AuthoredValue::List(
                items
                    .iter()
                    .map(|item| item.fill(values))
                    .collect::<Result<_, _>>()?,
            ),
            DefaultValue::Map(entries) => AuthoredValue::Map(fill_map(entries, values)?),
        })
    }
}

fn fill_map(
    entries: &[(String, DefaultValue)],
    values: &TemplateValues,
) -> Result<ValueMap, FillError> {
    let filled = entries
        .iter()
        .map(|(key, value)| Ok((key.clone(), value.fill(values)?)))
        .collect::<Result<Vec<_>, FillError>>()?;
    Ok(ValueMap::new(filled).expect("a YAML mapping holds each key once"))
}

fn source_map(entries: &[(String, DefaultValue)]) -> ValueMap {
    ValueMap::new(
        entries
            .iter()
            .map(|(key, value)| (key.clone(), value.source())),
    )
    .expect("a YAML mapping holds each key once")
}

/// What is wrong with a creation rule or the inbox.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CreationProblem {
    /// A template breaks the template grammar.
    Template(TemplateError),
    /// A rule's name is not an identifier.
    RuleName {
        /// The name, as written.
        name: String,
    },
    /// A declared variable's name is not an identifier.
    VariableName {
        /// The name, as written.
        name: String,
    },
    /// A variable is declared twice.
    VariableTwice {
        /// The name declared twice.
        name: String,
    },
    /// A template names a variable the rule does not declare.
    UndeclaredVariable {
        /// The variable named.
        name: String,
    },
    /// The target is no document path: what the document-path grammar
    /// refuses in it, judged on its literal text with each token standing as
    /// a plain value.
    Path(PathProblem),
    /// The target's literal text holds `:`, which is not portable in a file
    /// name: Windows reads it as a drive or a stream.
    Colon,
    /// The target holds `{{now}}` or `{{time}}` without `|slug`, and each
    /// writes the clock with a `:` in it.
    ClockWithColon {
        /// The token's name: `now` or `time`.
        token: String,
    },
    /// The target does not end in `.md`.
    NotMarkdown,
    /// The target's file name is `.md` alone.
    FileNameEmpty,
    /// The target holds `{{seq}}` more than once.
    SeqTwice,
    /// The target holds `{{seq}}` in a folder rather than in its file name.
    SeqOutsideFileName,
    /// A frontmatter default's key is empty.
    EmptyDefaultKey,
    /// A frontmatter default's key is `<<`, which reads back as a merge.
    MergeDefaultKey,
    /// A frontmatter default's key holds `{{`, and a key is not a template.
    TemplatedDefaultKey {
        /// The key, as written.
        key: String,
    },
    /// A template other than a target holds `{{seq}}`.
    SeqOutsideTarget,
    /// The inbox's target holds no `{{seq}}`.
    InboxWithoutSeq,
    /// The inbox's target names a variable.
    InboxVariable {
        /// The variable named.
        name: String,
    },
    /// A template reads a path capture, which only a schema rule's default
    /// or route reads.
    PathCapture {
        /// The capture read.
        name: String,
    },
}

impl fmt::Display for CreationProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        const IDENTIFIER: &str = "an ASCII letter or `_` followed by letters, digits, `_` or `-`";
        match self {
            CreationProblem::Template(error) => write!(formatter, "{error}"),
            CreationProblem::RuleName { name } => write!(
                formatter,
                "names the rule `{name}`, and a rule's name is {IDENTIFIER}"
            ),
            CreationProblem::VariableName { name } => write!(
                formatter,
                "declares the variable `{name}`, and a variable's name is {IDENTIFIER}"
            ),
            CreationProblem::VariableTwice { name } => {
                write!(formatter, "declares the variable `{name}` twice")
            }
            CreationProblem::UndeclaredVariable { name } => write!(
                formatter,
                "names the variable `{name}`, which the rule's `variables` does not declare"
            ),
            CreationProblem::Path(problem) => {
                write!(formatter, "names no document path: {problem}")
            }
            CreationProblem::Colon => {
                write!(formatter, "holds `:`, which is not portable in a file name")
            }
            CreationProblem::ClockWithColon { token } => write!(
                formatter,
                "holds `{{{{{token}}}}}`, which writes the clock with `:`, and `:` is not portable in a file name; `{{{{{token}|slug}}}}` writes it without"
            ),
            CreationProblem::NotMarkdown => write!(
                formatter,
                "does not end in `.md`, and a target names a Markdown document"
            ),
            CreationProblem::FileNameEmpty => {
                write!(formatter, "names a file with nothing before its `.md`")
            }
            CreationProblem::SeqTwice => write!(
                formatter,
                "holds `{{{{seq}}}}` more than once, and a document has one sequence number"
            ),
            CreationProblem::SeqOutsideFileName => write!(
                formatter,
                "holds `{{{{seq}}}}` in a folder, and a sequence number stands only in the file name"
            ),
            CreationProblem::EmptyDefaultKey => write!(
                formatter,
                "holds a default under an empty key, which names no field"
            ),
            CreationProblem::MergeDefaultKey => write!(
                formatter,
                "holds a default under `<<`, which a document's frontmatter reads as a merge rather than a field"
            ),
            CreationProblem::TemplatedDefaultKey { key } => write!(
                formatter,
                "holds a default under `{key}`, and a key is written as it lands, not filled as a template"
            ),
            CreationProblem::SeqOutsideTarget => write!(
                formatter,
                "holds `{{{{seq}}}}`, which stands only in a target"
            ),
            CreationProblem::InboxWithoutSeq => write!(
                formatter,
                "holds no `{{{{seq}}}}`, and the inbox numbers what it captures"
            ),
            CreationProblem::InboxVariable { name } => write!(
                formatter,
                "names the variable `{name}`, and the inbox is supplied no variables"
            ),
            CreationProblem::PathCapture { name } => write!(
                formatter,
                "reads the path capture `{name}`, and only a schema rule's default or route reads a path capture"
            ),
        }
    }
}

fn refusal(at: impl Into<String>, problem: CreationProblem) -> VaultSchemaError {
    VaultSchemaError::Creation {
        at: at.into(),
        problem,
    }
}

/// Reads the `creatable` section: each rule, by name.
pub(super) fn read_creatable(
    document: &Mapping,
) -> Result<BTreeMap<String, CreationRule>, VaultSchemaError> {
    let Some(value) = at(document, "creatable") else {
        return Ok(BTreeMap::new());
    };
    let Value::Mapping(rules) = value else {
        return Err(section_error(
            "creatable",
            "a mapping of rule name to rule",
            value,
        ));
    };
    rules
        .iter()
        .map(|(name, rule)| {
            let name = name
                .as_str()
                .ok_or_else(|| section_error("creatable", "a mapping keyed by rule name", name))?;
            if !is_identifier(name) {
                return Err(refusal(
                    "creatable",
                    CreationProblem::RuleName {
                        name: name.to_string(),
                    },
                ));
            }
            Ok((name.to_string(), read_rule(name, rule)?))
        })
        .collect()
}

fn read_rule(name: &str, rule: &Value) -> Result<CreationRule, VaultSchemaError> {
    let section = format!("creatable.{name}");
    let Value::Mapping(rule) = rule else {
        return Err(section_error(&section, "a mapping", rule));
    };
    known_keys_only(&section, rule, RULE_KEYS)?;
    let variables = read_variables(&section, rule)?;
    let declared: BTreeSet<&str> = variables.iter().map(String::as_str).collect();
    let uses_declared = |at: &str, template: &Template| {
        template
            .variables()
            .find(|variable| !declared.contains(variable))
            .map_or(Ok(()), |variable| {
                Err(refusal(
                    at,
                    CreationProblem::UndeclaredVariable {
                        name: variable.to_string(),
                    },
                ))
            })
    };

    let target_at = format!("{section}.target");
    let target = read_target(&target_at, rule)?;
    uses_declared(&target_at, target.template())?;

    let body = match at(rule, "body") {
        None => None,
        Some(value) => {
            let at_path = format!("{section}.body");
            let source = value
                .as_str()
                .ok_or_else(|| section_error(&at_path, "a string", value))?;
            let body = templated(&at_path, source)?;
            uses_declared(&at_path, &body)?;
            Some(body)
        }
    };

    let frontmatter_defaults = match at(rule, "frontmatter_defaults") {
        None => Vec::new(),
        Some(value) => {
            let at_path = format!("{section}.frontmatter_defaults");
            let Value::Mapping(defaults) = value else {
                return Err(section_error(&at_path, "a mapping", value));
            };
            read_default_entries(&at_path, defaults, &uses_declared)?
        }
    };

    Ok(CreationRule {
        name: name.to_string(),
        target,
        variables,
        frontmatter_defaults,
        body,
    })
}

fn read_variables(section: &str, rule: &Mapping) -> Result<Vec<String>, VaultSchemaError> {
    let Some(value) = at(rule, "variables") else {
        return Ok(Vec::new());
    };
    let at_path = format!("{section}.variables");
    let Value::Sequence(items) = value else {
        return Err(section_error(&at_path, "a sequence of names", value));
    };
    let mut variables: Vec<String> = Vec::new();
    for item in items {
        let name = item
            .as_str()
            .ok_or_else(|| section_error(&at_path, "a sequence of names", item))?;
        if !is_identifier(name) {
            return Err(refusal(
                &at_path,
                CreationProblem::VariableName {
                    name: name.to_string(),
                },
            ));
        }
        if variables.iter().any(|held| held == name) {
            return Err(refusal(
                &at_path,
                CreationProblem::VariableTwice {
                    name: name.to_string(),
                },
            ));
        }
        variables.push(name.to_string());
    }
    Ok(variables)
}

/// Reads the `target` of `mapping`, which `at_path` names.
fn read_target(at_path: &str, mapping: &Mapping) -> Result<Target, VaultSchemaError> {
    let Some(value) = at(mapping, "target") else {
        return Err(VaultSchemaError::Section {
            at: at_path.to_string(),
            wanted: "a target path",
            found: "absent".to_string(),
        });
    };
    let source = value
        .as_str()
        .ok_or_else(|| section_error(at_path, "a target path", value))?;
    let template = Template::parse(source)
        .map_err(|error| refusal(at_path, CreationProblem::Template(error)))?;
    reads_no_capture(at_path, &template)?;
    Target::read(template).map_err(|problem| refusal(at_path, problem))
}

/// Refuses a creation template that reads a path capture.
fn reads_no_capture(at_path: &str, template: &Template) -> Result<(), VaultSchemaError> {
    match template.path_captures().next() {
        Some(name) => Err(refusal(
            at_path,
            CreationProblem::PathCapture {
                name: name.to_string(),
            },
        )),
        None => Ok(()),
    }
}

/// `source` as a template that holds no `{{seq}}`, which stands only in a
/// target.
fn templated(at_path: &str, source: &str) -> Result<Template, VaultSchemaError> {
    let template = Template::parse(source)
        .map_err(|error| refusal(at_path, CreationProblem::Template(error)))?;
    if template.seq_count() > 0 {
        return Err(refusal(at_path, CreationProblem::SeqOutsideTarget));
    }
    reads_no_capture(at_path, &template)?;
    Ok(template)
}

type UsesDeclared<'a> = dyn Fn(&str, &Template) -> Result<(), VaultSchemaError> + 'a;

fn read_default_entries(
    at_path: &str,
    mapping: &Mapping,
    uses_declared: &UsesDeclared<'_>,
) -> Result<Vec<(String, DefaultValue)>, VaultSchemaError> {
    mapping
        .iter()
        .map(|(key, value)| {
            let key = key
                .as_str()
                .ok_or_else(|| section_error(at_path, "a mapping keyed by field name", key))?;
            if let Some(problem) = default_key_problem(key) {
                return Err(refusal(at_path, problem));
            }
            let value = read_default(&format!("{at_path}.{key}"), value, uses_declared)?;
            Ok((key.to_string(), value))
        })
        .collect()
}

/// Why `key` would not land in a document's frontmatter as the field it is
/// written as, or `None` where it would.
///
/// An empty key names no field. `<<` is the merge key, which norn's own
/// frontmatter reader expands, quoted or not, so a default under it would read
/// back as a merge. And a key is not a template, so a key holding `{{` is a
/// template written where none is read.
fn default_key_problem(key: &str) -> Option<CreationProblem> {
    if key.is_empty() {
        Some(CreationProblem::EmptyDefaultKey)
    } else if key == "<<" {
        Some(CreationProblem::MergeDefaultKey)
    } else if key.contains("{{") {
        Some(CreationProblem::TemplatedDefaultKey {
            key: key.to_string(),
        })
    } else {
        None
    }
}

/// One frontmatter default, which `at_path` names: every string scalar in it
/// read as a template.
fn read_default(
    at_path: &str,
    value: &Value,
    uses_declared: &UsesDeclared<'_>,
) -> Result<DefaultValue, VaultSchemaError> {
    Ok(match value {
        Value::Null => DefaultValue::Plain(AuthoredValue::Null),
        Value::Bool(value) => DefaultValue::Plain(AuthoredValue::Bool(*value)),
        Value::Number(number) => DefaultValue::Plain(if let Some(integer) = number.as_i64() {
            AuthoredValue::Integer(integer)
        } else if number.is_u64() {
            return Err(section_error(
                at_path,
                "an integer that fits a signed 64-bit integer",
                value,
            ));
        } else {
            number
                .as_f64()
                .and_then(|float| FiniteFloat::new(float).ok())
                .map(AuthoredValue::Float)
                .ok_or_else(|| section_error(at_path, "a finite number", value))?
        }),
        Value::String(source) => {
            let template = templated(at_path, source)?;
            uses_declared(at_path, &template)?;
            DefaultValue::Text(template)
        }
        Value::Sequence(items) => DefaultValue::List(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    read_default(&format!("{at_path}.{index}"), item, uses_declared)
                })
                .collect::<Result<_, _>>()?,
        ),
        Value::Mapping(entries) => {
            DefaultValue::Map(read_default_entries(at_path, entries, uses_declared)?)
        }
        Value::Tagged(_) => return Err(section_error(at_path, "a frontmatter value", value)),
    })
}

/// Reads the `inbox` section.
pub(super) fn read_inbox(document: &Mapping) -> Result<Option<Inbox>, VaultSchemaError> {
    let Some(value) = at(document, "inbox") else {
        return Ok(None);
    };
    let Value::Mapping(inbox) = value else {
        return Err(section_error("inbox", "a mapping", value));
    };
    known_keys_only("inbox", inbox, INBOX_KEYS)?;
    let target = read_target("inbox.target", inbox)?;
    if let Some(variable) = target.template().variables().next() {
        return Err(refusal(
            "inbox.target",
            CreationProblem::InboxVariable {
                name: variable.to_string(),
            },
        ));
    }
    if !target.has_seq() {
        return Err(refusal("inbox.target", CreationProblem::InboxWithoutSeq));
    }
    Ok(Some(Inbox { target }))
}
