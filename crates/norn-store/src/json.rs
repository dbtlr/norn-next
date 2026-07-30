//! The frontmatter projection — one value tree in, one canonical JSON text out.
//!
//! `documents.frontmatter` holds a JSON1 projection of the frontmatter block:
//! one column, queryable through SQLite's JSON functions, schema-independent
//! and carrying no invalidation key. There is deliberately **no field-per-row
//! table** beside it. Field rows are an entity-attribute-value shape, and every
//! query over one pays a join per field it reads while giving up the ability to
//! ask about a field's *structure* at all.
//!
//! # Projected, never authoritative
//!
//! The file is the authority for what a document's frontmatter says. This text
//! is a derived reading of it, kept because a reading in a column can be
//! queried and a file cannot. Two consequences follow and both are contract:
//!
//! - **The projection is one-way.** Key order is not preserved, so the value
//!   tree cannot be reconstructed from the column. Anything that has to render
//!   or edit frontmatter reads the file through the one parser; anything that
//!   has to *filter* on it reads this column.
//! - **A shape JSON cannot hold is projected to `null`**, which happens for a
//!   non-finite float. JSON has no NaN and no infinity, and inventing a
//!   spelling for them would put a value in the column that no JSON reader
//!   agrees about.
//!
//! # Canonical means the same value always writes the same bytes
//!
//! - **Map keys are sorted** by byte order, so a document whose fields were
//!   written in a different order projects identically. Document order is
//!   information, and it is information the *file* carries; a projection that
//!   kept it would make the column's bytes depend on something the column
//!   cannot be queried by.
//! - **A repeated key keeps its last value.** The value model does not produce
//!   one — a block with duplicate keys does not parse — so this is the
//!   canonicalization being total rather than a case anything relies on. It
//!   matches the model's own "insert replaces" reading of a repeated key.
//! - **Numbers are written in the shortest form that reads back exactly**, with
//!   integral floats keeping a `.0` so that a float never projects as something
//!   `json_type` calls an integer.
//! - **Strings escape exactly what JSON requires** and nothing else: the quote,
//!   the backslash, and the C0 controls. Every other scalar value is emitted as
//!   the UTF-8 it already is.

/// A frontmatter value, as the store takes it.
///
/// The seven shapes of the frontmatter value model and no eighth. It is the
/// store's own type rather than the text layer's: the store depends on no
/// parser, and the host maps one onto the other.
///
/// A map is a list of pairs rather than a sorted structure because the caller
/// hands over what it read, in the order it read it; sorting is
/// [`canonical_json`]'s job and happens once, where the bytes are written.
#[derive(Clone, Debug, PartialEq)]
pub enum FrontmatterValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Sequence(Vec<FrontmatterValue>),
    Map(Vec<(String, FrontmatterValue)>),
}

/// The canonical JSON text for one value.
pub fn canonical_json(value: &FrontmatterValue) -> String {
    let mut out = String::new();
    write_value(value, &mut out);
    out
}

fn write_value(value: &FrontmatterValue, out: &mut String) {
    match value {
        FrontmatterValue::Null => out.push_str("null"),
        FrontmatterValue::Bool(true) => out.push_str("true"),
        FrontmatterValue::Bool(false) => out.push_str("false"),
        FrontmatterValue::Int(number) => out.push_str(&number.to_string()),
        FrontmatterValue::Float(number) => write_float(*number, out),
        FrontmatterValue::String(text) => write_string(text, out),
        FrontmatterValue::Sequence(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        FrontmatterValue::Map(entries) => {
            let mut sorted: Vec<&(String, FrontmatterValue)> = entries.iter().collect();
            // A stable sort, so that a repeated key keeps the entries in the
            // order they arrived and the last of the run is the one written.
            sorted.sort_by(|left, right| left.0.cmp(&right.0));
            out.push('{');
            let mut written = 0usize;
            for (index, (key, value)) in sorted.iter().enumerate() {
                if sorted.get(index + 1).is_some_and(|next| next.0 == *key) {
                    continue;
                }
                if written > 0 {
                    out.push(',');
                }
                write_string(key, out);
                out.push(':');
                write_value(value, out);
                written += 1;
            }
            out.push('}');
        }
    }
}

/// A float in the shortest decimal form that reads back exactly, or `null` where
/// JSON has no spelling for it.
///
/// The form carries no exponent, whatever the magnitude: an extreme value writes
/// its digits out. That is the shortest form that reads back to the same double,
/// which is the property the projection needs — a compact exponent notation
/// would be a second spelling of the same number, and a column two equal values
/// disagree about is the thing canonicalization exists to prevent.
fn write_float(number: f64, out: &mut String) {
    if !number.is_finite() {
        out.push_str("null");
        return;
    }
    let written = number.to_string();
    out.push_str(&written);
    // `1.0` writes as `1`, which every JSON reader calls an integer. The
    // fractional marker is what keeps a float's shape in the projection.
    if !written.contains('.') {
        out.push_str(".0");
    }
}

fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            control if control < ' ' => {
                out.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}
