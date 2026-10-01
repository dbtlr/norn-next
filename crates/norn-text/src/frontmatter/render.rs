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
//! A collection is built from those proven scalars and keys in block style,
//! and is then proven whole: its entry, or its list item, is re-read and
//! compared against the value it came from, so an indentation or nesting
//! mistake refuses the same way a quoting one does.
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
use crate::span::trailing_break;
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
    ///
    /// A collection is proven whole, as a block value, and a collection that
    /// does not read back names its whole rendering as the text.
    NotRoundTrippable {
        text: String,
        context: ScalarContext,
    },
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderError::NotRoundTrippable { text, context } => write!(
                f,
                "no quoting style renders {text:?} so that it reads back unchanged as a {context}"
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

/// A scalar the model holds, borrowed: what a value span or a flow item is
/// written from.
///
/// A collection has no scalar view, so the renderers that write into a span
/// or a flow item cannot be handed one: a collection replaces a field's whole
/// entry ([`render_entry`]) or is a block-list item ([`render_block_item`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Scalar<'a> {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(&'a str),
}

impl<'a> Scalar<'a> {
    /// The scalar `value` is, or `None` for a collection.
    pub(crate) fn of(value: &'a Value) -> Option<Self> {
        match view(value) {
            View::Scalar(scalar) => Some(scalar),
            View::Sequence(_) | View::Map(_) => None,
        }
    }

    /// The scalars `items` are, or `None` where any of them is a collection.
    pub(crate) fn all(items: &'a [Value]) -> Option<Vec<Self>> {
        items.iter().map(Scalar::of).collect()
    }

    fn to_value(self) -> Value {
        match self {
            Scalar::Null => Value::Null,
            Scalar::Bool(value) => Value::Bool(value),
            Scalar::Int(number) => Value::Int(number),
            Scalar::Float(number) => Value::Float(number),
            Scalar::String(text) => Value::String(text.to_string()),
        }
    }

    /// The scalar spelled at quoting `rank`. A non-string scalar has one
    /// spelling, whatever the rank.
    fn spelled_at(self, rank: u8) -> String {
        match self {
            Scalar::Null => "~".to_string(),
            Scalar::Bool(true) => "true".to_string(),
            Scalar::Bool(false) => "false".to_string(),
            Scalar::Int(number) => number.to_string(),
            Scalar::Float(number) => render_float(number),
            Scalar::String(text) => render_at_rank(text, rank),
        }
    }
}

/// What a value is to a writer: a scalar it spells, or a collection it lays
/// out.
enum View<'a> {
    Scalar(Scalar<'a>),
    Sequence(&'a [Value]),
    Map(&'a Mapping),
}

fn view(value: &Value) -> View<'_> {
    match value {
        Value::Null => View::Scalar(Scalar::Null),
        Value::Bool(value) => View::Scalar(Scalar::Bool(*value)),
        Value::Int(number) => View::Scalar(Scalar::Int(*number)),
        Value::Float(number) => View::Scalar(Scalar::Float(*number)),
        Value::String(text) => View::Scalar(Scalar::String(text)),
        Value::Sequence(items) => View::Sequence(items),
        Value::Map(map) => View::Map(map),
    }
}

/// The bytes that replace a field's `value_range` with `scalar`, keeping the
/// author's quoting where the new value permits it and upgrading where it
/// does not.
pub(crate) fn render_scalar_in_span(
    scalar: Scalar<'_>,
    original: ScalarStyle,
) -> Result<String, RenderError> {
    render_scalar(scalar, original.rank(), ScalarContext::Block)
}

/// A sequence written inline: `[one, two]`. Each item is verified as a flow
/// item, where a comma splits and a bracket breaks the document.
///
/// Only a flat sequence is written inline: a sequence holding a collection is
/// written in block style ([`render_entry`]) whatever style it replaces.
pub(crate) fn render_flow_sequence(items: &[Scalar<'_>]) -> Result<String, RenderError> {
    let mut out = String::from("[");
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(&render_scalar(*item, RANK_PLAIN, ScalarContext::Flow)?);
    }
    out.push(']');
    Ok(out)
}

/// Whether `value` is a sequence or a map.
pub(crate) fn is_collection(value: &Value) -> bool {
    matches!(value, Value::Sequence(_) | Value::Map(_))
}

/// Whether `value` is nested: a map, or a sequence holding a collection.
pub(crate) fn is_nested(value: &Value) -> bool {
    match value {
        Value::Map(_) => true,
        Value::Sequence(items) => items.iter().any(is_collection),
        _ => false,
    }
}

/// A whole `field: <value>` entry, its terminator included, for any value
/// the model holds.
///
/// A scalar is written on the key line at the least quoting that reads back.
/// A collection is written in block style, its entries or items two spaces
/// deeper than the key, and each level of nesting two spaces deeper again; an
/// empty one is written `field: []` or `field: {}`, because a bare `field:`
/// line reads back as null, not as the empty collection it was written as.
///
/// A collection's entry is proven whole: it is re-read and refused with
/// [`RenderError::NotRoundTrippable`] unless it reads back as exactly
/// `field` holding `value`.
pub(crate) fn render_entry(
    field: &str,
    value: &Value,
    line_ending: LineEnding,
) -> Result<String, RenderError> {
    let mut out = render_key(field)?;
    out.push(':');
    write_value(value, Slot::Key, "", line_ending.as_str(), &mut out)?;
    if is_collection(value) {
        let read = match reparse(&out) {
            Some(Value::Map(map)) if map.len() == 1 => map.get(field).cloned(),
            _ => None,
        };
        prove(read.as_ref() == Some(value), &out)?;
    }
    Ok(out)
}

/// One block-list item — `{indent}- item{terminator}` — written as
/// [`render_entry`] writes a value: a scalar at the least quoting that reads
/// back as `item`, a collection in block style. A collection's first entry or
/// item shares the `-` line (`- k: v`, `- - a`) and the rest of it sits two
/// spaces past `indent`, so the item is one block whatever it holds.
///
/// The item is proven as an entry is: re-read alone, it is a list holding
/// exactly `item`.
pub(crate) fn render_block_item(
    item: &Value,
    indent: &str,
    terminator: &str,
) -> Result<String, RenderError> {
    let mut out = format!("{indent}-");
    write_value(item, Slot::Item, indent, terminator, &mut out)?;
    if is_collection(item) {
        let read = match reparse(&out) {
            Some(Value::Sequence(items)) if items.len() == 1 => items.into_iter().next(),
            _ => None,
        };
        prove(read.as_ref() == Some(item), &out)?;
    }
    Ok(out)
}

/// Where [`write_value`] writes a value: after a mapping key's `:`, or after
/// a block item's `-`.
#[derive(Clone, Copy)]
enum Slot {
    Key,
    Item,
}

/// Write what follows the `:` or the `-` that opens `value`, at `indent` —
/// the indent of the line holding that indicator — its terminator included.
fn write_value(
    value: &Value,
    slot: Slot,
    indent: &str,
    terminator: &str,
    out: &mut String,
) -> Result<(), RenderError> {
    let child = format!("{indent}  ");
    let mut body = String::new();
    // Whether `body` is block lines at `child`, each terminated, rather than
    // one inline value.
    let block = match view(value) {
        View::Sequence([]) => {
            body.push_str("[]");
            false
        }
        View::Map(map) if map.is_empty() => {
            body.push_str("{}");
            false
        }
        View::Sequence(items) => {
            for item in items {
                body.push_str(&child);
                body.push('-');
                write_value(item, Slot::Item, &child, terminator, &mut body)?;
            }
            true
        }
        View::Map(map) => {
            for (key, entry) in map.iter() {
                body.push_str(&child);
                body.push_str(&render_key(key)?);
                body.push(':');
                write_value(entry, Slot::Key, &child, terminator, &mut body)?;
            }
            true
        }
        View::Scalar(scalar) => {
            body.push_str(&render_scalar(scalar, RANK_PLAIN, ScalarContext::Block)?);
            false
        }
    };
    match (slot, block) {
        // Under a key, a block collection starts on the next line.
        (Slot::Key, true) => {
            out.push_str(terminator);
            out.push_str(&body);
        }
        // After a `-`, its first line is the item's own line.
        (Slot::Item, true) => {
            out.push(' ');
            out.push_str(&body[child.len()..]);
        }
        (_, false) => {
            out.push(' ');
            out.push_str(&body);
            out.push_str(terminator);
        }
    }
    Ok(())
}

/// `rendered`, refused unless it `reads_back`.
fn prove(reads_back: bool, rendered: &str) -> Result<(), RenderError> {
    if reads_back {
        Ok(())
    } else {
        Err(RenderError::NotRoundTrippable {
            text: rendered.to_string(),
            context: ScalarContext::Block,
        })
    }
}

/// A field name in key position, quoted only where the round-trip requires it.
///
/// A plain identifier renders as itself. `#foo` would otherwise become a
/// comment and `a: b` invalid YAML, so both escalate; `123` and `true` escalate
/// because they would stop being strings.
pub(crate) fn render_key(field: &str) -> Result<String, RenderError> {
    render_scalar(Scalar::String(field), RANK_PLAIN, ScalarContext::Key)
}

/// Emit `scalar` at the least-quoted rank at or above `start` that reads back
/// as exactly that scalar in `context`, or refuse. A non-string scalar has one
/// spelling, and it is still verified: a float that rendered as `1` would read
/// back as an integer.
fn render_scalar(
    scalar: Scalar<'_>,
    start: u8,
    context: ScalarContext,
) -> Result<String, RenderError> {
    let value = scalar.to_value();
    let ranks = match scalar {
        Scalar::String(_) => start..=RANK_DOUBLE,
        _ => RANK_PLAIN..=RANK_PLAIN,
    };
    for rank in ranks {
        let rendered = scalar.spelled_at(rank);
        if reparse_in_context(&rendered, context).as_ref() == Some(&value) {
            return Ok(rendered);
        }
    }
    Err(RenderError::NotRoundTrippable {
        text: scalar.spelled_at(RANK_PLAIN),
        context,
    })
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
/// document is a question about the vault, not about its syntax. A collection
/// emits block style, nested ones two spaces deeper per level, and an empty
/// one emits `key: []` or `key: {}`. Each collection is re-read before it is
/// returned, and one that does not read back refuses with
/// [`RenderError::NotRoundTrippable`].
pub fn render_document(
    fields: &Mapping,
    body: &str,
    line_ending: LineEnding,
) -> Result<String, RenderError> {
    let terminator = line_ending.as_str();
    let mut out = format!("---{terminator}");
    for (field, value) in fields.iter() {
        out.push_str(&render_entry(field, value, line_ending)?);
    }
    out.push_str("---");
    out.push_str(terminator);
    if !body.is_empty() {
        out.push_str(body);
        // Whether the body's last line is already terminated is the crate's
        // break rule, so a body ending in a lone `\r` ends a line and gets no
        // second terminator welded onto it.
        if trailing_break(body).is_none() {
            out.push_str(terminator);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::Scalar;
    use crate::value::{Mapping, Value};

    /// **Only a scalar has a scalar view**, so a collection cannot reach the
    /// renderers that write a value span or a flow item.
    #[test]
    fn a_collection_has_no_scalar_view() {
        assert_eq!(Scalar::of(&Value::Int(1)), Some(Scalar::Int(1)));
        assert_eq!(Scalar::of(&"a".into()), Some(Scalar::String("a")));
        assert_eq!(Scalar::of(&Value::Sequence(Vec::new())), None);
        assert_eq!(Scalar::of(&Value::Map(Mapping::new())), None);
    }
}
