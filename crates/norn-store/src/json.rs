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
//! - **A repeated key keeps its last value.** No document reaches this with
//!   one: a block writing one key twice, however the two were spelled, is
//!   refused where it is read, so a projected document's map answers each of
//!   its keys once. This is the canonicalization being total over the values a
//!   caller can compose rather than a case anything a document produces relies
//!   on, and the value it keeps is the one a second write through a map that
//!   replaces in place would leave.
//! - **Numbers are written as plain decimal digits, never in exponent
//!   notation**, in the shortest such spelling that reads back to the same
//!   value, with integral floats keeping a `.0` so that a float never projects
//!   as something `json_type` calls an integer. Choosing between plain and
//!   exponent form by magnitude would give one number two spellings — which is
//!   what canonicalization exists to prevent — so an extreme value is long
//!   rather than compact.
//! - **Strings escape exactly what JSON requires** and nothing else: the quote,
//!   the backslash, and the C0 controls. Every other scalar value is emitted as
//!   the UTF-8 it already is.
//!
//! # Nesting is bounded
//!
//! A value nested deeper than [`MAX_FRONTMATTER_DEPTH`] is **refused**, because
//! the alternative is a column no reader can read: SQLite's JSON1 functions
//! parse to a bounded depth, so past it a write that succeeded would produce
//! bytes `json_valid` rejects — which the store's own verification reports as
//! damage, permanently, through the rung that discards the database. Refusing
//! also keeps the writer's own recursion shallow.
//!
//! The bound sits above what the text layer can produce (libyaml stops at 128)
//! and below what SQLite accepts, so nothing a parsed document carries reaches
//! it. The API is public, though, and a caller composing a value tree is not
//! bounded by a parser at all.
//!
//! Which is why **every walk over a value is iterative**, the measurement and the
//! value's own [`Drop`] alike. A value past the bound is refused rather than
//! written, and it then still has to be *freed*: a recursive drop would recurse
//! once per level and abort the process on a value this API accepted, which would
//! make the refusal a crash one statement later rather than an error a caller
//! reads.

/// A frontmatter value, as the store takes it.
///
/// The seven shapes of the frontmatter value model and no eighth. It is the
/// store's own type rather than the text layer's: the store depends on no
/// parser, and the host maps one onto the other.
///
/// A map is a list of pairs rather than a sorted structure because the caller
/// hands over what it read, in the order it read it; sorting is
/// [`canonical_json`]'s job and happens once, where the bytes are written.
///
/// **Its `Drop` is iterative**, because a value this type can hold is not bounded
/// by what this crate will project: nesting deeper than
/// [`MAX_FRONTMATTER_DEPTH`] is refused, and the refused value is still the
/// caller's to drop. A derived recursive drop would recurse once per level and
/// abort the process on a deep one — turning a refusal into a crash, at the far
/// end of the call that refused. Dropping is the one traversal that happens
/// whether or not anything asked for it; the derived `Clone`, `Debug` and
/// `PartialEq` recurse, and a caller that asks one of them for a value past the
/// bound is asking for that depth.
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

impl Drop for FrontmatterValue {
    fn drop(&mut self) {
        let mut pending = Vec::new();
        take_children(self, &mut pending);
        // Each value taken out of the worklist has had its own children moved
        // into the worklist, so the drop that runs when it leaves this scope has
        // an empty container to free and recurses no further.
        while let Some(mut value) = pending.pop() {
            take_children(&mut value, &mut pending);
        }
    }
}

/// Move a value's children onto `pending`, leaving an empty container behind.
///
/// A map's keys are dropped here rather than carried: a `String` holds no
/// [`FrontmatterValue`], so it is not part of the walk.
fn take_children(value: &mut FrontmatterValue, pending: &mut Vec<FrontmatterValue>) {
    match value {
        FrontmatterValue::Sequence(items) => pending.append(items),
        FrontmatterValue::Map(entries) => {
            pending.extend(std::mem::take(entries).into_iter().map(|(_, value)| value));
        }
        _ => {}
    }
}

/// How deeply a frontmatter projection may nest.
///
/// Above what any parsed document can carry, below what SQLite's JSON1 reader
/// accepts. See the module documentation for why the projection is bounded at
/// all.
pub const MAX_FRONTMATTER_DEPTH: usize = 256;

/// The canonical JSON text for one value, or a refusal where it nests deeper
/// than [`MAX_FRONTMATTER_DEPTH`].
pub fn canonical_json(value: &FrontmatterValue) -> Result<String, crate::StoreError> {
    let depth = depth(value);
    if depth > MAX_FRONTMATTER_DEPTH {
        return Err(crate::StoreError::Bound {
            what: "a frontmatter projection's nesting",
            limit: MAX_FRONTMATTER_DEPTH,
            given: depth,
        });
    }
    let mut out = String::new();
    write_value(value, &mut out);
    Ok(out)
}

/// How deeply a value nests: a scalar is 1, a container is one more than its
/// deepest member, and an empty container is 1.
///
/// Walked with an explicit stack and stopped one past the bound. Measuring
/// recursively would be the same overflow the bound exists to refuse, and
/// measuring a pathological tree exactly would cost the walk the refusal is
/// meant to avoid — so a value deeper than the bound reports the bound plus one
/// rather than its true depth.
fn depth(value: &FrontmatterValue) -> usize {
    let mut deepest = 0;
    let mut pending = vec![(value, 1_usize)];
    while let Some((value, depth)) = pending.pop() {
        deepest = deepest.max(depth);
        if depth > MAX_FRONTMATTER_DEPTH {
            return depth;
        }
        match value {
            FrontmatterValue::Sequence(items) => {
                pending.extend(items.iter().map(|item| (item, depth + 1)));
            }
            FrontmatterValue::Map(entries) => {
                pending.extend(entries.iter().map(|(_, value)| (value, depth + 1)));
            }
            _ => {}
        }
    }
    deepest
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

/// A float as plain decimal digits, or `null` where JSON has no spelling for it.
///
/// The form carries no exponent, whatever the magnitude: an extreme value writes
/// its digits out. It is the shortest *plain decimal* spelling that reads back to
/// the same double, which is the property the projection needs — a compact
/// exponent notation would be a second spelling of the same number, and a column
/// two equal values disagree about is the thing canonicalization exists to
/// prevent.
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
