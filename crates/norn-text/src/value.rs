//! The vault value model — what a frontmatter value is allowed to be.
//!
//! Seven shapes, and no eighth: null, bool, integer, float, string, sequence,
//! and a string-keyed map. The model is derived from what a consumer of a
//! parsed value can hold, not from what a frontmatter dialect can express: no
//! shape here is a YAML shape, and a caller holding a [`Value`] holds nothing
//! it has to know YAML to read. The *style* vocabulary beside it — see
//! [`crate::ValueStyle`] and [`crate::ScalarContext`] — is YAML's, and says so:
//! a host that wants to preserve how a value was written is asking a question
//! about the dialect the document is in.
//!
//! # Scalar resolution is the YAML 1.2 core schema
//!
//! Which bytes read as which shape is a stated contract, pinned by tests, and
//! not an accident of the parser behind the seam. `publish: no` is the string
//! `"no"`, not `false`; `0777` is a string, not octal; `12:34:56` is a string,
//! not a sexagesimal integer; `1_000` and `2026-07-29` are strings. `true`,
//! `false`, `null`, `~`, decimal integers, `0x`/`0o` integers and
//! exponent-shaped floats resolve to their core-schema shapes. Changing this
//! is a deliberate contract change, visible as a suite change.
//!
//! # Expressiveness outside the model is stripped once, here
//!
//! A frontmatter block may carry constructs the model has no shape for. Each
//! is resolved at this boundary, loudly, and never carried further:
//!
//! - **A non-string key** — `1: x`, `true: y`, `[a]: x`, `~: v` — has no
//!   addressable form, so its entry is dropped with a
//!   `frontmatter-non-string-key` diagnostic. Coercing it to the string `"1"`
//!   would invent a field the document does not contain.
//! - **An explicit tag** — `!foo bar` — is dropped and its value kept, with a
//!   `frontmatter-tag-stripped` diagnostic.
//! - **An integer past `i64` but inside `u64`** is carried as a float, with a
//!   `frontmatter-integer-out-of-range` diagnostic. Past `u64`, or below
//!   `i64`, the block does not parse at all: the refusal is
//!   `frontmatter-parse-failed`, and no value exists to carry.
//! - **Anchors and aliases** are expanded by the parser before a value exists,
//!   so there is nothing here to strip: the model holds the expansion. The
//!   marker bytes themselves are never rewritten, because the field layer
//!   refuses an in-place edit of a value that begins with `&`, `*` or `!`.
//! - **A merge key** — `<<: *base` — is a directive rather than a field, and
//!   is expanded the same way and for the same reason: the extraction folds
//!   the merged mapping in before a value exists, so `<<` is never a field and
//!   never a phantom one. Its bytes are never rewritten either — a block
//!   carrying one has no trustworthy per-field split, so every field edit in
//!   it refuses.
//! - **Duplicate keys** are not a value-model question at all: the block is
//!   not well-formed, so it does not parse and no value exists.

use std::fmt::{self, Write as _};

use crate::diagnostic::Diagnostic;

/// A frontmatter value.
#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Sequence(Vec<Value>),
    Map(Mapping),
}

impl Value {
    /// The name of this value's shape, for a message that has to say what it
    /// found.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::String(_) => "string",
            Value::Sequence(_) => "sequence",
            Value::Map(_) => "map",
        }
    }

    /// True for null, bool, int, float and string — the shapes that occupy a
    /// single YAML scalar.
    pub fn is_scalar(&self) -> bool {
        !matches!(self, Value::Sequence(_) | Value::Map(_))
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_sequence(&self) -> Option<&[Value]> {
        match self {
            Value::Sequence(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&Mapping> {
        match self {
            Value::Map(map) => Some(map),
            _ => None,
        }
    }
}

/// Equality over the value model, with floats compared by
/// [`f64::total_cmp`]'s ordering rather than IEEE 754's.
///
/// This crate proves an edit by comparing the value it read back against the
/// value it was given, so a value that is not equal to itself is a value no
/// edit can be proven for. IEEE equality makes `.nan` exactly that: a document
/// holding one could not be edited at all, and this crate could not re-emit a
/// document it had just read. Total ordering fixes it — every value equals
/// itself, `.nan` included.
///
/// The other place the two differ is signed zero, and total ordering is the
/// answer there too: `0.0` and `-0.0` are written differently, read back
/// differently, and so are different values here. An emission of `-0.0` that
/// read back as `0.0` would be a silent rewrite, and comparing by bits is what
/// catches it.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Null, Value::Null) => true,
            (Value::Bool(left), Value::Bool(right)) => left == right,
            (Value::Int(left), Value::Int(right)) => left == right,
            (Value::Float(left), Value::Float(right)) => left.total_cmp(right).is_eq(),
            (Value::String(left), Value::String(right)) => left == right,
            (Value::Sequence(left), Value::Sequence(right)) => left == right,
            (Value::Map(left), Value::Map(right)) => left == right,
            _ => false,
        }
    }
}

impl From<&str> for Value {
    fn from(text: &str) -> Self {
        Value::String(text.to_string())
    }
}

impl From<String> for Value {
    fn from(text: String) -> Self {
        Value::String(text)
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Value::Bool(value)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Value::Int(value)
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Value::Float(value)
    }
}

/// A string-keyed map that remembers the order its entries arrived in.
///
/// Document order is information — it is the order a rendered document emits
/// fields in — so the map keeps it rather than sorting keys away. Keys are
/// unique: inserting an existing key replaces its value in place.
///
/// Lookup is a scan, which is the right shape for what this holds: a
/// frontmatter block is a handful of fields, and a hash map over that many
/// short strings costs more in hashing and allocation than the scan does in
/// comparisons — while giving up the ordering that makes an edit preserve key
/// order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mapping {
    entries: Vec<(String, Value)>,
}

impl Mapping {
    pub fn new() -> Self {
        Mapping::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Insert `value` at `key`, replacing an existing entry in place and
    /// appending a new one at the end. Returns the replaced value.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<Value>) -> Option<Value> {
        let key = key.into();
        let value = value.into();
        match self.entries.iter_mut().find(|(name, _)| *name == key) {
            Some(slot) => Some(std::mem::replace(&mut slot.1, value)),
            None => {
                self.entries.push((key, value));
                None
            }
        }
    }

    /// Append an entry whose key is known to be new, skipping the scan
    /// `insert` does to find an existing one. The parse boundary is the caller:
    /// a block with two entries under one key is not well-formed and never
    /// reaches the value model.
    pub(crate) fn append_unique(&mut self, key: String, value: Value) {
        debug_assert!(!self.contains_key(&key), "append_unique on a repeated key");
        self.entries.push((key, value));
    }

    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let index = self.entries.iter().position(|(name, _)| name == key)?;
        Some(self.entries.remove(index).1)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_str(), value))
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(key, _)| key.as_str())
    }
}

impl<K: Into<String>, V: Into<Value>> FromIterator<(K, V)> for Mapping {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut map = Mapping::new();
        for (key, value) in iter {
            map.insert(key, value);
        }
        map
    }
}

impl<'a> IntoIterator for &'a Mapping {
    type Item = (&'a str, &'a Value);
    type IntoIter = Box<dyn Iterator<Item = (&'a str, &'a Value)> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

impl From<Mapping> for Value {
    fn from(map: Mapping) -> Self {
        Value::Map(map)
    }
}

impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(items: Vec<T>) -> Self {
        Value::Sequence(items.into_iter().map(Into::into).collect())
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => f.write_str("null"),
            Value::Bool(value) => write!(f, "{value}"),
            Value::Int(value) => write!(f, "{value}"),
            Value::Float(value) => write!(f, "{value}"),
            Value::String(text) => write!(f, "{text}"),
            Value::Sequence(items) => {
                f.write_str("[")?;
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_str("]")
            }
            Value::Map(map) => {
                f.write_str("{")?;
                for (index, (key, value)) in map.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{key}: {value}")?;
                }
                f.write_str("}")
            }
        }
    }
}

/// What a conversion into the value model had to strip, if anything.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct StripReport {
    /// Entries dropped because their key is not a string. A dropped key makes
    /// the block's field spans untrustworthy: the scanner still sees the line,
    /// the value no longer names it, and absorbing that line into a
    /// neighbour's range is how a remove deletes an unrelated field.
    pub(crate) dropped_keys: usize,
}

impl StripReport {
    pub(crate) fn is_clean(self) -> bool {
        self.dropped_keys == 0
    }
}

/// Convert a parsed YAML value into the vault value model, reporting every
/// construct the model has no shape for.
///
/// `path` is a scratch buffer naming where the walk currently is. It is
/// extended and truncated in place rather than rebuilt per node, so a document
/// with no strippable construct in it pays nothing to be able to name one.
pub(crate) fn from_yaml(
    value: serde_yaml::Value,
    path: &mut String,
    diagnostics: &mut Vec<Diagnostic>,
    report: &mut StripReport,
) -> Value {
    match value {
        serde_yaml::Value::Null => Value::Null,
        serde_yaml::Value::Bool(value) => Value::Bool(value),
        serde_yaml::Value::Number(number) => number_from_yaml(number, path, diagnostics),
        serde_yaml::Value::String(text) => Value::String(text),
        serde_yaml::Value::Sequence(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (index, item) in items.into_iter().enumerate() {
                let mark = path.len();
                let _ = write!(path, "[{index}]");
                out.push(from_yaml(item, path, diagnostics, report));
                path.truncate(mark);
            }
            Value::Sequence(out)
        }
        serde_yaml::Value::Mapping(mapping) => {
            let mut map = Mapping::new();
            for (key, value) in mapping {
                let Some(key) = key.as_str().map(str::to_string) else {
                    report.dropped_keys += 1;
                    diagnostics.push(
                        Diagnostic::warning(
                            "frontmatter-non-string-key",
                            "an entry keyed by a non-string was dropped; the vault value model \
                             addresses fields by string key",
                        )
                        .with_detail(location(path)),
                    );
                    continue;
                };
                let mark = path.len();
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(&key);
                let value = from_yaml(value, path, diagnostics, report);
                path.truncate(mark);
                map.append_unique(key, value);
            }
            Value::Map(map)
        }
        serde_yaml::Value::Tagged(tagged) => {
            diagnostics.push(
                Diagnostic::warning(
                    "frontmatter-tag-stripped",
                    "an explicit YAML tag was dropped and its value kept; the vault value model \
                     carries no tags",
                )
                .with_detail(format!("`{}` {}", tagged.tag, location(path))),
            );
            from_yaml(tagged.value, path, diagnostics, report)
        }
    }
}

/// Where in the block a diagnostic is about, for a reader who has to find it.
fn location(path: &str) -> String {
    if path.is_empty() {
        "at the top level".to_string()
    } else {
        format!("at `{path}`")
    }
}

fn number_from_yaml(
    number: serde_yaml::Number,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Value {
    if let Some(value) = number.as_i64() {
        return Value::Int(value);
    }
    if let Some(value) = number.as_u64() {
        diagnostics.push(
            Diagnostic::warning(
                "frontmatter-integer-out-of-range",
                "an integer outside the range of a 64-bit signed integer is carried as a float",
            )
            .with_detail(format!("`{value}` {}", location(path))),
        );
        return Value::Float(value as f64);
    }
    Value::Float(number.as_f64().unwrap_or(f64::NAN))
}
