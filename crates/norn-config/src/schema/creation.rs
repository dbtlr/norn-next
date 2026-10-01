//! Creation rules and the inbox: how the schema says a new document is made.
//!
//! A **creation rule** is named, and names a templated target path, the
//! variables a caller must supply, frontmatter defaults and a body. The
//! **inbox** is the one place untyped capture lands, and names a target alone.
//! Both are written in the grammar of [`Template`].
//!
//! **Everything about where a token may stand is judged at schema read.** A
//! target ends in `.md`, is relative, and stays inside the vault — no leading
//! `/`, no empty, `.` or `..` segment, no `\` — judged on its literal text and
//! where its tokens stand, since a token's value is not known until a document
//! is made. `{{seq}}` stands only in a target, at most once, and only in its
//! file name. A `{{var.NAME}}` anywhere in a rule names a variable the rule
//! declares. The inbox's target carries `{{seq}}` and names no variable, since
//! untyped capture is supplied nothing. What a token's value may be is judged
//! when the rule is filled, which is not this module's.
//!
//! **A rule derives nothing.** No row a document's derivation writes reads a
//! creation rule or the inbox, so neither is a term of
//! [`VaultSchema::rederives_documents`](super::VaultSchema::rederives_documents).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_yaml::{Mapping, Value};

use norn_wire::{AuthoredValue, FiniteFloat, ValueMap};

use super::template::{Part, Slot, Template, TemplateError, Token, is_identifier};
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
        if template.as_str().starts_with('/') {
            return Err(CreationProblem::Absolute);
        }
        if template
            .parts()
            .iter()
            .any(|part| matches!(part, Part::Literal(text) if text.contains('\\')))
        {
            return Err(CreationProblem::Backslash);
        }
        let segments = segments(&template);
        for segment in &segments {
            if segment.is_empty() {
                return Err(CreationProblem::EmptySegment);
            }
            if let Some(text) = literal(segment)
                && (text == "." || text == "..")
            {
                return Err(CreationProblem::DotSegment { segment: text });
            }
        }
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
enum DefaultValue {
    /// Null, a boolean or a number.
    Plain(AuthoredValue),
    Text(Template),
    List(Vec<DefaultValue>),
    Map(Vec<(String, DefaultValue)>),
}

impl DefaultValue {
    /// The value as the schema writes it.
    fn source(&self) -> AuthoredValue {
        match self {
            DefaultValue::Plain(value) => value.clone(),
            DefaultValue::Text(template) => AuthoredValue::string(template.as_str()),
            DefaultValue::List(items) => AuthoredValue::list(items.iter().map(Self::source)),
            DefaultValue::Map(entries) => AuthoredValue::Map(source_map(entries)),
        }
    }
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
    /// The target starts with `/`.
    Absolute,
    /// The target's literal text holds `\`.
    Backslash,
    /// The target holds an empty path segment: a `//`, or a `/` at its end.
    EmptySegment,
    /// The target holds a `.` or `..` segment.
    DotSegment {
        /// The segment, as written.
        segment: String,
    },
    /// The target does not end in `.md`.
    NotMarkdown,
    /// The target's file name is `.md` alone.
    FileNameEmpty,
    /// The target holds `{{seq}}` more than once.
    SeqTwice,
    /// The target holds `{{seq}}` in a folder rather than in its file name.
    SeqOutsideFileName,
    /// A template other than a target holds `{{seq}}`.
    SeqOutsideTarget,
    /// The inbox's target holds no `{{seq}}`.
    InboxWithoutSeq,
    /// The inbox's target names a variable.
    InboxVariable {
        /// The variable named.
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
            CreationProblem::Absolute => write!(
                formatter,
                "starts with `/`, and a target is relative to the vault root"
            ),
            CreationProblem::Backslash => write!(
                formatter,
                "holds `\\`, and a target separates its folders with `/` alone"
            ),
            CreationProblem::EmptySegment => write!(
                formatter,
                "holds an empty path segment, and every segment of a target names something"
            ),
            CreationProblem::DotSegment { segment } => write!(
                formatter,
                "holds the segment `{segment}`, and a target names no `.` or `..` segment, so it stays inside the vault"
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
    Target::read(template).map_err(|problem| refusal(at_path, problem))
}

/// `source` as a template that holds no `{{seq}}`, which stands only in a
/// target.
fn templated(at_path: &str, source: &str) -> Result<Template, VaultSchemaError> {
    let template = Template::parse(source)
        .map_err(|error| refusal(at_path, CreationProblem::Template(error)))?;
    if template.seq_count() > 0 {
        return Err(refusal(at_path, CreationProblem::SeqOutsideTarget));
    }
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
            let value = read_default(&format!("{at_path}.{key}"), value, uses_declared)?;
            Ok((key.to_string(), value))
        })
        .collect()
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
