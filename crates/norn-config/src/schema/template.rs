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
//!
//! # Filling
//!
//! [`Template::fill`] turns a template into text from a [`TemplateValues`]:
//! the variables a caller supplied, one [`LocalTimestamp`], and the sequence
//! number where one is allocated. **A fill reads no clock.** The caller reads
//! the clock once and hands the reading over, so every template one plan
//! fills states the same instant, and the same values fill the same text on
//! every machine. A fill here never refuses for what a value holds; what a
//! target's path refuses is [`Target::fill`](super::Target::fill)'s.

use std::collections::BTreeMap;
use std::fmt;

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use norn_wire::PathProblem;

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

    /// The text this template fills to under `values`.
    ///
    /// Every value is taken as it is, after its filter: a body or a
    /// frontmatter string holds whatever it is supplied. A variable `values`
    /// does not supply, and `{{seq}}` where it carries no sequence number, are
    /// refused.
    pub fn fill(&self, values: &TemplateValues) -> Result<String, FillError> {
        self.parts.iter().try_fold(String::new(), |mut text, part| {
            match part {
                Part::Literal(literal) => text.push_str(literal),
                Part::Token(token) => text.push_str(&token.fill(values)?),
            }
            Ok(text)
        })
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
    /// The value this token fills to under `values`, after its filter.
    pub(crate) fn fill(&self, values: &TemplateValues) -> Result<String, FillError> {
        let value = match &self.slot {
            Slot::Seq => values.seq.ok_or(FillError::NoSeq)?.to_string(),
            Slot::Var(name) => values
                .variables
                .get(name)
                .cloned()
                .ok_or_else(|| FillError::MissingVariable { name: name.clone() })?,
            Slot::Now => values.at.rfc3339(),
            Slot::Date => values.at.date(),
            Slot::Time => values.at.time(),
        };
        Ok(if self.slug { slug(&value) } else { value })
    }

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

/// The token as it is written between its braces: `var.title|slug`.
impl fmt::Display for Token {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.slot {
            Slot::Seq => formatter.write_str("seq")?,
            Slot::Var(name) => write!(formatter, "var.{name}")?,
            Slot::Now => formatter.write_str("now")?,
            Slot::Date => formatter.write_str("date")?,
            Slot::Time => formatter.write_str("time")?,
        }
        if self.slug {
            formatter.write_str("|slug")?;
        }
        Ok(())
    }
}

/// `text` as a slug: normalized, lowercased, its words kept, and every run of
/// anything else between them one `-`, with none leading or trailing.
///
/// The text is put in Unicode's canonical composition (NFC) first, so text
/// that is canonically equal slugs the same: `cafe` with a combining acute
/// and `café` are one slug. A word is letters, digits and combining marks, all
/// Unicode's, so `Straße` keeps its `ß`, `日本語` its characters, `हिन्दी` its
/// vowel sign and virama, and the dot `İ` lowercases to stays on its `i`. A
/// mark continues the word it follows, and one that follows no word is part
/// of the run between words. Text holding no letter and no digit slugs to
/// nothing.
fn slug(text: &str) -> String {
    let mut slug = String::with_capacity(text.len());
    let mut separated = false;
    for character in text.nfc().flat_map(char::to_lowercase) {
        let in_word = character.is_alphabetic()
            || character.is_numeric()
            || (is_combining_mark(character) && !separated && !slug.is_empty());
        if in_word {
            if separated && !slug.is_empty() {
                slug.push('-');
            }
            separated = false;
            slug.push(character);
        } else {
            separated = true;
        }
    }
    slug
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

/// One clock reading in the local zone: the calendar day, the time to the
/// second, and the offset from UTC that zone stood at.
///
/// It is a reading, not a clock. The host reads the clock once for a plan and
/// builds one of these; every template the plan fills reads the same one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalTimestamp {
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    offset_minutes: i16,
}

impl LocalTimestamp {
    /// The reading of `year-month-day hour:minute:second` at
    /// `offset_minutes` east of UTC, or the refusal of one no calendar and
    /// clock have: a year outside `0..=9999`, which `{{date}}` cannot write
    /// in four digits; a day the month does not have; a time outside the day
    /// or a leap second; or an offset a day or more from UTC.
    pub fn new(
        year: i32,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
        offset_minutes: i16,
    ) -> Result<Self, NotALocalTimestamp> {
        let in_calendar = (0..=9999).contains(&year)
            && (1..=12).contains(&month)
            && day >= 1
            && i64::from(day) <= super::typed::days_in_month(year.into(), month.into());
        let in_clock = hour < 24 && minute < 60 && second < 60;
        let in_offset = offset_minutes.unsigned_abs() < 24 * 60;
        if !(in_calendar && in_clock && in_offset) {
            return Err(NotALocalTimestamp);
        }
        Ok(LocalTimestamp {
            year,
            month,
            day,
            hour,
            minute,
            second,
            offset_minutes,
        })
    }

    /// `{{date}}`: `YYYY-MM-DD`.
    fn date(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// `{{time}}`: `HH:MM`.
    fn time(self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }

    /// `{{now}}`: RFC 3339 to the second with the local offset, which is
    /// written `+00:00` at UTC rather than `Z`, so every reading states its
    /// offset one way.
    fn rfc3339(self) -> String {
        let sign = if self.offset_minutes < 0 { '-' } else { '+' };
        let offset = self.offset_minutes.unsigned_abs();
        format!(
            "{}T{:02}:{:02}:{:02}{sign}{:02}:{:02}",
            self.date(),
            self.hour,
            self.minute,
            self.second,
            offset / 60,
            offset % 60
        )
    }
}

/// A reading no calendar and clock have.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NotALocalTimestamp;

impl fmt::Display for NotALocalTimestamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "a local timestamp is a day the calendar has in years 0 to 9999, a time inside that day, and an offset less than a day from UTC",
        )
    }
}

impl std::error::Error for NotALocalTimestamp {}

/// What a template is filled from: the variables a caller supplied, one
/// clock reading, and the sequence number where one is allocated.
#[derive(Clone, Debug)]
pub struct TemplateValues {
    variables: BTreeMap<String, String>,
    at: LocalTimestamp,
    seq: Option<u64>,
}

impl TemplateValues {
    /// `variables` by name, read at `at`, with no sequence number.
    pub fn new(variables: BTreeMap<String, String>, at: LocalTimestamp) -> Self {
        TemplateValues {
            variables,
            at,
            seq: None,
        }
    }

    /// The same values, numbered `seq`.
    #[must_use]
    pub fn with_seq(mut self, seq: u64) -> Self {
        self.seq = Some(seq);
        self
    }

    pub(crate) fn seq(&self) -> Option<u64> {
        self.seq
    }
}

/// Why a template does not fill.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FillError {
    /// A token names a variable no value is supplied for. A schema read
    /// already holds every variable a rule names to one it declares, so this
    /// is a caller that did not supply a declared one.
    MissingVariable {
        /// The variable, as the rule declares it.
        name: String,
    },
    /// `{{seq}}` is filled with no sequence number supplied.
    NoSeq,
    /// A value would break the target's path.
    UnsafeValue {
        /// The token, as written between its braces: `var.title|slug`.
        token: String,
        /// The value it filled to, after its filter.
        value: String,
        /// How it would break the path.
        problem: UnsafeValue,
    },
    /// The whole path a target fills to is no document path: a value joined
    /// the literal text beside it into something no single value is, such as
    /// a `..` segment.
    NotADocumentPath {
        /// The path the target fills to, with `{{seq}}` as written where the
        /// target is numbered.
        path: String,
        /// What the document-path grammar refuses in it.
        problem: PathProblem,
    },
}

/// How a value would break a target's path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsafeValue {
    /// It holds `/` or `\`, so it would name a folder the target does not.
    Separator,
    /// It holds `:`, which is not portable in a file name.
    Colon,
    /// It is `.` or `..`, so it would name the folder it stands in or climb
    /// out of it.
    DotSegment,
    /// It is empty, so it would leave a segment or a file name with nothing
    /// where it stands.
    Empty,
}

impl fmt::Display for FillError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FillError::MissingVariable { name } => {
                write!(formatter, "no value is supplied for the variable `{name}`")
            }
            FillError::NoSeq => formatter.write_str("`{{seq}}` is filled with no sequence number"),
            FillError::UnsafeValue {
                token,
                value,
                problem,
            } => {
                let why = match problem {
                    UnsafeValue::Separator => "holds `/` or `\\`, which would name another folder",
                    UnsafeValue::Colon => "holds `:`, which is not portable in a file name",
                    UnsafeValue::DotSegment => {
                        "is a `.` or `..` segment, which would leave the folder it stands in"
                    }
                    UnsafeValue::Empty => "is empty, which would leave nothing where it stands",
                };
                write!(
                    formatter,
                    "`{{{{{token}}}}}` fills the target with `{value}`, which {why}"
                )
            }
            FillError::NotADocumentPath { path, problem } => {
                write!(
                    formatter,
                    "the target fills to {path:?}, which is no document path: {problem}"
                )
            }
        }
    }
}

impl std::error::Error for FillError {}

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
