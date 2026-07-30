//! Emitting frontmatter bytes, and proving they read back.
//!
//! # Verified or refused
//!
//! Correctness here is decided by the YAML standard, not by a list of
//! hazardous shapes somebody maintains. Every scalar this module emits is
//! re-parsed **in the lexical context it will actually live in** and compared
//! against the value it came from; a rendering that does not reproduce its
//! value exactly is not returned. Quoting escalates plain → single → double
//! until one round-trips, and if none does the emission refuses with
//! [`RenderError::NotRoundTrippable`] rather than handing back an unproven
//! render for somebody else to write to disk.
//!
//! Context is load-bearing because the same bytes mean different things in
//! different places. In a block value (`k: <here>`) a comma is an ordinary
//! character; in a flow item (`k: [<here>]`) it splits the item in two, and a
//! bracket makes the whole document unparseable, which takes every field in
//! the block with it. In key position (`<here>: x`) a leading `#` turns the
//! line into a comment, an embedded `: ` splits it into nested mappings, and
//! `123`, `true` and `null` stop being strings.
//!
//! # Minimal by default, and never a downgrade
//!
//! An emission starts at the least-quoted style its origin permits and climbs
//! only as far as the round-trip demands, so a plain value stays plain. An
//! explicit quote style is a floor: a single-quoted value is never rewritten
//! plain, and a double-quoted one is never rewritten single.

use std::fmt;

use crate::frontmatter::fields::{ValueStyle, reparse};
use crate::line_ending::LineEnding;
use crate::value::{Mapping, Value};

/// The YAML lexical context a scalar is emitted into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarContext {
    /// A mapping value: `key: <scalar>`.
    Block,
    /// An item of a flow collection: `key: [<scalar>]`.
    Flow,
    /// A mapping key: `<scalar>: value`.
    Key,
}

impl fmt::Display for ScalarContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ScalarContext::Block => "block value",
            ScalarContext::Flow => "flow item",
            ScalarContext::Key => "mapping key",
        })
    }
}

/// Why frontmatter bytes could not be emitted.
#[derive(Debug, Clone, PartialEq)]
pub enum RenderError {
    /// No quoting style renders this text so that it reads back unchanged in
    /// this context. The refusal that replaces writing an unproven render.
    NotRoundTrippable {
        text: String,
        context: ScalarContext,
    },
    /// A map, or a collection nested inside a sequence. The frontmatter edit
    /// surface writes scalars and flat sequences.
    NonScalarValue { kind: &'static str },
    /// A sequence was offered for a field currently holding a scalar. Remove
    /// the field and write it afresh rather than silently restyling it.
    SequenceIntoScalar,
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderError::NotRoundTrippable { text, context } => write!(
                f,
                "no quoting style renders {text:?} so that it reads back unchanged as a {context}"
            ),
            RenderError::NonScalarValue { kind } => {
                write!(f, "a {kind} value cannot be written here")
            }
            RenderError::SequenceIntoScalar => f.write_str(
                "a sequence cannot replace a scalar field; remove the field and write it afresh",
            ),
        }
    }
}

impl std::error::Error for RenderError {}

/// Quoting ranks, ordered by strictness. Escalation only climbs.
const RANK_PLAIN: u8 = 0;
const RANK_SINGLE: u8 = 1;
const RANK_DOUBLE: u8 = 2;

/// The quoting a scalar already carries, and so the floor its replacement is
/// emitted at.
///
/// Only the three scalar spellings exist here, and that is the point: a value
/// written as a block scalar or a collection has no value span, so there is no
/// in-place replacement to render and no way to ask for one. What the field
/// layer cannot name, this layer cannot be handed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScalarStyle {
    Plain,
    SingleQuoted,
    DoubleQuoted,
}

impl ScalarStyle {
    /// The scalar spelling `style` carries, or `None` when the style names no
    /// replaceable span.
    pub(crate) fn of(style: ValueStyle) -> Option<Self> {
        match style {
            // A stubbed key has no quoting yet, so its replacement starts at
            // the least-quoted rank like a plain value does.
            ValueStyle::Plain | ValueStyle::EmptyValue => Some(ScalarStyle::Plain),
            ValueStyle::SingleQuoted => Some(ScalarStyle::SingleQuoted),
            ValueStyle::DoubleQuoted => Some(ScalarStyle::DoubleQuoted),
            _ => None,
        }
    }

    fn rank(self) -> u8 {
        match self {
            ScalarStyle::Plain => RANK_PLAIN,
            ScalarStyle::SingleQuoted => RANK_SINGLE,
            ScalarStyle::DoubleQuoted => RANK_DOUBLE,
        }
    }
}

/// The bytes that replace a field's `value_range`, keeping the author's
/// quoting where the new value permits it and upgrading where it does not.
pub(crate) fn render_scalar_in_span(
    value: &Value,
    original: ScalarStyle,
) -> Result<String, RenderError> {
    match value {
        Value::Sequence(_) => Err(RenderError::SequenceIntoScalar),
        Value::Map(_) => Err(RenderError::NonScalarValue { kind: "map" }),
        scalar => render_scalar(scalar, original.rank(), ScalarContext::Block),
    }
}

/// A sequence written inline: `[one, two]`. Each item is verified as a flow
/// item, where a comma splits and a bracket breaks the document.
pub(crate) fn render_flow_sequence(items: &[Value]) -> Result<String, RenderError> {
    let mut out = String::from("[");
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(&render_scalar(item, RANK_PLAIN, ScalarContext::Flow)?);
    }
    out.push(']');
    Ok(out)
}

/// A whole `field: <sequence>` entry, its terminator included.
///
/// An empty sequence emits `field: []`: a bare `field:` line reads back as
/// null, not as the empty list it was written as.
pub(crate) fn render_sequence_entry(
    field: &str,
    items: &[Value],
    line_ending: LineEnding,
) -> Result<String, RenderError> {
    let key = render_key(field)?;
    if items.is_empty() {
        return Ok(format!("{key}: []{}", line_ending.as_str()));
    }
    Ok(format!(
        "{key}:{}{}",
        line_ending.as_str(),
        render_block_items(items, line_ending)?
    ))
}

/// A sequence's block items — `  - one` lines at a two-space indent, each
/// terminated.
pub(crate) fn render_block_items(
    items: &[Value],
    line_ending: LineEnding,
) -> Result<String, RenderError> {
    let mut out = String::new();
    for item in items {
        out.push_str("  - ");
        out.push_str(&render_scalar(item, RANK_PLAIN, ScalarContext::Block)?);
        out.push_str(line_ending.as_str());
    }
    Ok(out)
}

/// A whole `field: <scalar>` entry, its terminator included.
pub(crate) fn render_scalar_entry(
    field: &str,
    value: &Value,
    line_ending: LineEnding,
) -> Result<String, RenderError> {
    Ok(format!(
        "{}: {}{}",
        render_key(field)?,
        render_scalar(value, RANK_PLAIN, ScalarContext::Block)?,
        line_ending.as_str()
    ))
}

/// A field name in key position, quoted only where the round-trip requires it.
///
/// A plain identifier renders as itself. `#foo` would otherwise become a
/// comment and `a: b` invalid YAML, so both escalate; `123` and `true` escalate
/// because they would stop being strings.
pub(crate) fn render_key(field: &str) -> Result<String, RenderError> {
    render_scalar(
        &Value::String(field.to_string()),
        RANK_PLAIN,
        ScalarContext::Key,
    )
}

/// Emit `value` at the least-quoted rank at or above `start` that reads back
/// as exactly `value` in `context`, or refuse.
fn render_scalar(value: &Value, start: u8, context: ScalarContext) -> Result<String, RenderError> {
    let text = match value {
        Value::String(text) => text.as_str(),
        Value::Sequence(_) => return Err(RenderError::NonScalarValue { kind: "sequence" }),
        Value::Map(_) => return Err(RenderError::NonScalarValue { kind: "map" }),
        // A non-string scalar has one spelling, and it is still verified: a
        // float that rendered as `1` would read back as an integer.
        other => {
            let rendered = render_non_string(other);
            return if reparse_in_context(&rendered, context).as_ref() == Some(other) {
                Ok(rendered)
            } else {
                Err(RenderError::NotRoundTrippable {
                    text: rendered,
                    context,
                })
            };
        }
    };

    for rank in start..=RANK_DOUBLE {
        let rendered = render_at_rank(text, rank);
        if reparse_in_context(&rendered, context).as_ref() == Some(value) {
            return Ok(rendered);
        }
    }
    Err(RenderError::NotRoundTrippable {
        text: text.to_string(),
        context,
    })
}

fn render_non_string(value: &Value) -> String {
    match value {
        Value::Null => "~".to_string(),
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        Value::Int(number) => number.to_string(),
        Value::Float(number) => render_float(*number),
        Value::String(_) | Value::Sequence(_) | Value::Map(_) => unreachable!("scalar only"),
    }
}

/// A float spelled so it reads back as a float: `1` would read back as an
/// integer, and YAML spells the non-finite values `.nan`, `.inf` and `-.inf`.
fn render_float(number: f64) -> String {
    if number.is_nan() {
        return ".nan".to_string();
    }
    if number.is_infinite() {
        return if number.is_sign_positive() {
            ".inf".to_string()
        } else {
            "-.inf".to_string()
        };
    }
    format!("{number:?}")
}

fn render_at_rank(text: &str, rank: u8) -> String {
    match rank {
        RANK_PLAIN => text.to_string(),
        RANK_SINGLE => format!("'{}'", text.replace('\'', "''")),
        _ => format!("\"{}\"", escape_double_quoted(text)),
    }
}

/// Read `rendered` back as the value it would be in `context`.
fn reparse_in_context(rendered: &str, context: ScalarContext) -> Option<Value> {
    match context {
        ScalarContext::Block => match reparse(&format!("k: {rendered}"))? {
            Value::Map(map) if map.len() == 1 => map.get("k").cloned(),
            _ => None,
        },
        // A flow item reads back only if it is the sole element: a value that
        // would split on `,` or `]` fails here and escalates to a quote.
        ScalarContext::Flow => match reparse(&format!("k: [{rendered}]"))? {
            Value::Map(map) if map.len() == 1 => match map.get("k") {
                Some(Value::Sequence(items)) if items.len() == 1 => Some(items[0].clone()),
                _ => None,
            },
            _ => None,
        },
        // A key reads back only if the mapping has exactly the one entry, its
        // value is the sentinel, and its key is a string.
        ScalarContext::Key => match reparse(&format!("{rendered}: x\n"))? {
            Value::Map(map) if map.len() == 1 => match map.iter().next() {
                Some((key, Value::String(sentinel))) if sentinel == "x" => {
                    Some(Value::String(key.to_string()))
                }
                _ => None,
            },
            _ => None,
        },
    }
}

/// Escape a string for a double-quoted scalar. Double-quoted is the terminal
/// rank, so this has to produce bytes that read back exactly for every input —
/// control characters in particular, which YAML forbids literally inside
/// quotes, and the separators NEL, LS and PS, which fold to a space if left
/// bare.
fn escape_double_quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\0' => out.push_str("\\0"),
            '\u{07}' => out.push_str("\\a"),
            '\u{08}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{0b}' => out.push_str("\\v"),
            '\u{0c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            '\u{1b}' => out.push_str("\\e"),
            '\u{85}' => out.push_str("\\N"),
            '\u{2028}' => out.push_str("\\L"),
            '\u{2029}' => out.push_str("\\P"),
            // Every remaining control character is at most 0xFF, so two hex
            // digits always suffice.
            ch if ch.is_control() => out.push_str(&format!("\\x{:02X}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out
}

/// Write a whole document from scratch: a frontmatter block holding `fields`
/// in the order they are given, then `body`.
///
/// Every line uses `line_ending`. Fields are emitted exactly as offered — a
/// null field emits `key: ~`, because whether an unset field belongs in a
/// document is a question about the vault, not about its syntax. A sequence
/// emits block style, and an empty sequence emits `key: []`.
pub fn render_document(
    fields: &Mapping,
    body: &str,
    line_ending: LineEnding,
) -> Result<String, RenderError> {
    let terminator = line_ending.as_str();
    let mut out = format!("---{terminator}");
    for (field, value) in fields.iter() {
        match value {
            Value::Sequence(items) => {
                out.push_str(&render_sequence_entry(field, items, line_ending)?);
            }
            Value::Map(_) => return Err(RenderError::NonScalarValue { kind: "map" }),
            scalar => out.push_str(&render_scalar_entry(field, scalar, line_ending)?),
        }
    }
    out.push_str("---");
    out.push_str(terminator);
    if !body.is_empty() {
        out.push_str(body);
        if !body.ends_with('\n') {
            out.push_str(terminator);
        }
    }
    Ok(out)
}
