//! The template grammar a creation rule is written in.
//!
//! A template is text with tokens in it, and a token is one of five values
//! written between `{{` and `}}`:
//!
//! - `{{seq}}` — the document's sequence number, a plain integer.
//! - `{{var.NAME}}` — a variable the rule declares, supplied when a document
//!   is made.
//! - `{{now}}` — the instant of making, in RFC 3339 to the second with the
//!   local offset: `2026-10-01T19:00:00+02:00`.
//! - `{{date}}` — the local calendar day of that instant: `2026-10-01`.
//! - `{{time}}` — the local hour and minute of that instant: `19:00`.
//!
//! Any token may carry the one filter, `|slug`: `{{var.title|slug}}`.
//!
//! **The grammar is closed and refused at read.** A token or a filter this
//! grammar does not name, a variable whose name is not an identifier, and a
//! `{{` with no `}}` after it are refusals when the schema is read, so a
//! template that reads is one every fill understands. There is no escape for
//! a literal `{{`, and no configurable format: a template means one text for
//! one set of values.
//!
//! **Parsing is all this module does at schema read.** Where a token may stand
//! — `{{seq}}` only in a target's file name, a variable only where the rule
//! declares it — is the creation rule's to judge, because it depends on which
//! part of a rule the template is.

use std::fmt;

/// A template's text, read into the literal runs and tokens it is made of.
///
/// The source is kept as written, because it is what `describe` reports and
/// what an author recognizes; the parts are what a fill walks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Template {
    source: String,
    parts: Vec<Part>,
}

/// One run of a template: text taken as written, or a token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Part {
    Literal(String),
    Token(Token),
}

/// One token: the value it stands for, and whether `|slug` is applied to it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Token {
    pub(crate) slot: Slot,
    pub(crate) slug: bool,
}

/// The value a token stands for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Slot {
    Seq,
    Var(String),
    Now,
    Date,
    Time,
}

impl Template {
    /// Reads `source` into the template it spells, or the reason it spells
    /// none.
    pub fn parse(source: &str) -> Result<Self, TemplateError> {
        let mut parts = Vec::new();
        let mut rest = source;
        let mut offset = 0;
        while let Some(open) = rest.find("{{") {
            if open > 0 {
                parts.push(Part::Literal(rest[..open].to_string()));
            }
            let inner = open + 2;
            let Some(close) = rest[inner..].find("}}") else {
                return Err(TemplateError::Unclosed { at: offset + open });
            };
            parts.push(Part::Token(Token::read(&rest[inner..inner + close])?));
            let consumed = inner + close + 2;
            offset += consumed;
            rest = &rest[consumed..];
        }
        if !rest.is_empty() {
            parts.push(Part::Literal(rest.to_string()));
        }
        Ok(Template {
            source: source.to_string(),
            parts,
        })
    }

    /// The template as it is written.
    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// Every variable a token of this template names, in the order written,
    /// once for each token naming it.
    pub fn variables(&self) -> impl Iterator<Item = &str> {
        self.tokens().filter_map(|token| match &token.slot {
            Slot::Var(name) => Some(name.as_str()),
            _ => None,
        })
    }

    /// How many `{{seq}}` tokens the template holds.
    pub fn seq_count(&self) -> usize {
        self.tokens()
            .filter(|token| token.slot == Slot::Seq)
            .count()
    }

    pub(crate) fn parts(&self) -> &[Part] {
        &self.parts
    }

    fn tokens(&self) -> impl Iterator<Item = &Token> {
        self.parts.iter().filter_map(|part| match part {
            Part::Token(token) => Some(token),
            Part::Literal(_) => None,
        })
    }
}

impl Token {
    /// Reads the text between a token's braces.
    fn read(inner: &str) -> Result<Self, TemplateError> {
        let (name, slug) = match inner.split_once('|') {
            None => (inner, false),
            Some((name, "slug")) => (name, true),
            Some((_, filter)) => {
                return Err(TemplateError::UnknownFilter {
                    token: inner.to_string(),
                    filter: filter.to_string(),
                });
            }
        };
        let slot = match name {
            "seq" => Slot::Seq,
            "now" => Slot::Now,
            "date" => Slot::Date,
            "time" => Slot::Time,
            _ => match name.strip_prefix("var.") {
                Some(variable) if is_identifier(variable) => Slot::Var(variable.to_string()),
                Some(variable) => {
                    return Err(TemplateError::VariableName {
                        name: variable.to_string(),
                    });
                }
                None => {
                    return Err(TemplateError::UnknownToken {
                        token: inner.to_string(),
                    });
                }
            },
        };
        Ok(Token { slot, slug })
    }
}

/// Whether `name` is a name a variable or a creation rule may have: an ASCII
/// letter or `_`, then ASCII letters, digits, `_` and `-`.
///
/// Conservative on purpose. A name crosses a command line (`new --as task`,
/// `--var project=norn`) and sits inside `{{var.NAME}}`, and a name with
/// spaces, dots, braces or `|` in it would collide with one of those
/// spellings; widening the grammar later breaks no schema that reads today.
pub(crate) fn is_identifier(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|rest| rest.is_ascii_alphanumeric() || rest == '_' || rest == '-')
}

/// Why text is not a template.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TemplateError {
    /// A `{{` with no `}}` after it. There is no escape for a literal `{{`.
    Unclosed {
        /// The byte offset of the `{{`.
        at: usize,
    },
    /// A token this grammar does not name.
    UnknownToken {
        /// The text between the braces.
        token: String,
    },
    /// A filter other than `slug`.
    UnknownFilter {
        /// The text between the braces.
        token: String,
        /// The filter, as written after the `|`.
        filter: String,
    },
    /// `{{var.NAME}}` with a name that is not an identifier.
    VariableName {
        /// The name, as written.
        name: String,
    },
}

impl fmt::Display for TemplateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TemplateError::Unclosed { at } => write!(
                formatter,
                "opens a token at byte {at} that no `}}}}` closes, and a template has no literal `{{{{`"
            ),
            TemplateError::UnknownToken { token } => write!(
                formatter,
                "holds `{{{{{token}}}}}`, and a token is `{{{{seq}}}}`, `{{{{var.NAME}}}}`, `{{{{now}}}}`, `{{{{date}}}}` or `{{{{time}}}}`"
            ),
            TemplateError::UnknownFilter { token, filter } => write!(
                formatter,
                "holds `{{{{{token}}}}}`, whose filter `{filter}` is not one; the one filter is `slug`"
            ),
            TemplateError::VariableName { name } => write!(
                formatter,
                "names the variable `{name}`, and a variable's name is an ASCII letter or `_` followed by letters, digits, `_` or `-`"
            ),
        }
    }
}

impl std::error::Error for TemplateError {}
