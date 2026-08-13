//! The value model — what a frontmatter value is allowed to be.
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
//!   [`FrontmatterNonStringKey`](crate::DiagnosticCode::FrontmatterNonStringKey)
//!   diagnostic. Coercing it to the string `"1"` would invent a field the
//!   document does not contain.
//! - **An explicit tag** — `!foo bar` — is dropped and what it tagged is kept,
//!   with a
//!   [`FrontmatterTagStripped`](crate::DiagnosticCode::FrontmatterTagStripped)
//!   diagnostic. A key carries one the same way a value does: a tag is no part
//!   of a key's name, so `!foo k` is the field `k` and the note says which of
//!   the two the tag was on.
//! - **An integer past `i64` but inside `u64`** is carried as a float, with a
//!   [`FrontmatterIntegerOutOfRange`](crate::DiagnosticCode::FrontmatterIntegerOutOfRange)
//!   diagnostic. Past `u64`, or below `i64`, the block does not parse at all:
//!   the refusal is
//!   [`FrontmatterParseFailed`](crate::DiagnosticCode::FrontmatterParseFailed),
//!   and no value exists to carry.
//! - **Anchors and aliases** are expanded by the parser before a value exists,
//!   so there is nothing here to strip: the model holds the expansion. The
//!   marker bytes themselves are never rewritten, because the field layer
//!   refuses an in-place edit of a value that begins with `&`, `*` or `!`.
//! - **A merge key** — `<<: *base` — is a directive rather than a field, and
//!   is expanded the same way and for the same reason: the extraction folds
//!   the merged mapping in before a value exists, so `<<` is never a field and
//!   never a phantom one. Explicit keys keep the positions the document wrote
//!   them in and a key only the merge contributes is appended after them. The
//!   directive's own bytes are never rewritten, because its line belongs to no
//!   field. What costs a block its edits is a key the merge *introduces*: it
//!   is a parsed key no line can be attributed to, so the block has no
//!   trustworthy per-field split and every field edit in it refuses. A merge
//!   contributing only keys the block already writes leaves it editable. A
//!   directive written under a tag — `!x <<` — is the directive still, and one
//!   no line of the block can be attributed to, so it refuses the block rather
//!   than reaching the model as the field the name would otherwise make.
//! - **A repeated key** costs the block its read rather than reaching the
//!   model. Two spellings of one key are one key: the parser refuses `k: 1`
//!   beside `k: 2` itself, and a pair it holds apart — `!x k: 1` beside
//!   `k: 2`, whose key tag this boundary strips — is refused here, where the
//!   two collapse. Either way the block is
//!   [`FrontmatterParseFailed`](crate::DiagnosticCode::FrontmatterParseFailed)
//!   and no value exists, so no mapping in the model answers one key twice.

use std::collections::{HashMap, HashSet};
use std::fmt::{self, Write as _};

use crate::diagnostic::{Diagnostic, DiagnosticCode};

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

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(text) => Some(text),
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
///
/// A scan is right for a lookup and wrong for a *walk*: a caller that resolves
/// every key of a mapping pays one scan per key, which is quadratic in key
/// count. A walk of a large mapping resolves its keys through a by-key view of
/// it instead, built once inside this module.
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

    /// Append an entry at the end, skipping the scan `insert` does to find an
    /// existing key.
    ///
    /// Uniqueness is settled once per mapping by [`Mapping::repeated_key`]
    /// rather than per call — a scan per append is the quadratic this method
    /// exists to avoid — and a mapping whose keys repeat is refused there
    /// rather than carried.
    pub(crate) fn append(&mut self, key: String, value: Value) {
        self.entries.push((key, value));
    }

    /// The first key this mapping holds more than one entry under.
    ///
    /// Past [`HASH_ABOVE`] entries the pass is over a table, which keeps the
    /// answer linear in key count where the square of a scan is what a read of
    /// a block full of keys would pay. At or under it the pass compares
    /// instead: a handful of short strings is fewer comparisons than a table
    /// costs to allocate and hash into, and this runs on every mapping of every
    /// block that is read.
    pub(crate) fn repeated_key(&self) -> Option<&str> {
        if self.entries.len() > HASH_ABOVE {
            let mut seen: HashSet<&str> = HashSet::with_capacity(self.entries.len());
            return self
                .entries
                .iter()
                .map(|(key, _)| key.as_str())
                .find(|key| !seen.insert(key));
        }
        self.entries
            .iter()
            .enumerate()
            .find(|(index, (key, _))| self.entries[..*index].iter().any(|(seen, _)| seen == key))
            .map(|(_, (key, _))| key.as_str())
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

/// A by-key view of a [`Mapping`], borrowed from it, for a caller that resolves
/// every key rather than a few.
///
/// [`Mapping::get`] is a scan over the mapping's entries, so resolving `n` keys
/// through it costs `n` scans — quadratic in the key count, and the block's byte
/// bound caps how far that goes without flattening it. Hashing the entries once
/// makes every later lookup constant, so a walk of the whole mapping stays
/// linear in its key count. What that costs is one borrowed key reference and
/// one borrowed value reference per entry, held for the walk.
///
/// At [`HASH_ABOVE`] entries or fewer the view scans the mapping instead. A
/// block of a handful of fields is what an ordinary document carries, and there
/// the table's allocation and its hash per entry cost more than the comparisons
/// they save.
///
/// A key resolves to the **first** entry carrying it, which is what
/// [`Mapping::get`] answers, so a walk through this view and a walk of
/// [`Mapping::get`] calls read the same value for every key. That agreement is
/// the point: this is a faster route to an answer, never a different one.
///
/// It is a view and not a cache: it borrows the mapping, so a mapping cannot be
/// mutated while one stands and no index can outlive or disagree with the
/// entries it was built from.
pub(crate) enum KeyIndex<'a> {
    Scan(&'a Mapping),
    Hashed(HashMap<&'a str, &'a Value>),
}

/// The entry count above which a question about a mapping's keys is answered
/// through a hash table rather than a scan — both for a by-key view of it and
/// for [`Mapping::repeated_key`].
///
/// Both arms answer identically, so what this value picks is cost and never
/// correctness. It sits where the two routes cost about the same in an
/// optimized build: under it the comparisons a scan makes are cheaper than
/// allocating a table and hashing every key into it, and over it the scan's
/// square is what dominates. An ordinary document's block — title, dates,
/// status, tags — holds fewer fields than this, so the common read allocates no
/// table at all.
const HASH_ABOVE: usize = 16;

impl<'a> KeyIndex<'a> {
    pub(crate) fn of(map: &'a Mapping) -> Self {
        if map.len() <= HASH_ABOVE {
            return KeyIndex::Scan(map);
        }
        let mut by_key: HashMap<&'a str, &'a Value> = HashMap::with_capacity(map.len());
        for (key, value) in map.iter() {
            by_key.entry(key).or_insert(value);
        }
        KeyIndex::Hashed(by_key)
    }

    pub(crate) fn get(&self, key: &str) -> Option<&'a Value> {
        match self {
            KeyIndex::Scan(map) => map.get(key),
            KeyIndex::Hashed(by_key) => by_key.get(key).copied(),
        }
    }

    pub(crate) fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
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

/// Convert a parsed YAML value into the value model, reporting every
/// construct the model has no shape for.
///
/// `path` is a scratch buffer naming where the walk currently is. It is
/// extended and truncated in place rather than rebuilt per node, so a document
/// with no strippable construct in it pays nothing to be able to name one.
///
/// The error is the account of a key the block writes twice, which is the one
/// construct this boundary refuses rather than strips: the conversion collapses
/// spellings the parser holds apart, so the value it would return could answer
/// one key two ways.
pub(crate) fn from_yaml(
    value: serde_yaml::Value,
    path: &mut String,
    diagnostics: &mut Vec<Diagnostic>,
    report: &mut StripReport,
) -> Result<Value, String> {
    match value {
        serde_yaml::Value::Null => Ok(Value::Null),
        serde_yaml::Value::Bool(value) => Ok(Value::Bool(value)),
        serde_yaml::Value::Number(number) => Ok(number_from_yaml(number, path, diagnostics)),
        serde_yaml::Value::String(text) => Ok(Value::String(text)),
        serde_yaml::Value::Sequence(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (index, item) in items.into_iter().enumerate() {
                let mark = path.len();
                let _ = write!(path, "[{index}]");
                out.push(from_yaml(item, path, diagnostics, report)?);
                path.truncate(mark);
            }
            Ok(Value::Sequence(out))
        }
        serde_yaml::Value::Mapping(mapping) => {
            let mut map = Mapping::new();
            for (key, value) in mapping {
                let Some(name) = key.as_str().map(str::to_string) else {
                    report.dropped_keys += 1;
                    diagnostics.push(
                        Diagnostic::warning(
                            DiagnosticCode::FrontmatterNonStringKey,
                            "an entry keyed by a non-string was dropped; the value model \
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
                path.push_str(&name);
                // A tag on a key names no part of the key, so the entry keeps
                // its name and the tag is reported the way a tag on a value is.
                if let serde_yaml::Value::Tagged(tagged) = &key {
                    diagnostics.push(tag_stripped(&tagged.tag, "a key", path));
                }
                let value = from_yaml(value, path, diagnostics, report)?;
                path.truncate(mark);
                map.append(name, value);
            }
            // A tag on a key is dropped by the strip above, so `!x k` and `k`
            // arrive as distinct nodes and leave as one name. The parser refuses
            // the pair it can see; the pair only this collapse produces is
            // refused here, and both refusals cost the block its read.
            //
            // Recovery at entry level — keeping the first entry under a
            // repeated key and reading the rest of the block — belongs to a
            // replacement of the parser behind this seam, which is what decides
            // what a duplicate is on the other route.
            if let Some(key) = map.repeated_key() {
                return Err(duplicate_entry(path, key));
            }
            Ok(Value::Map(map))
        }
        serde_yaml::Value::Tagged(tagged) => {
            diagnostics.push(tag_stripped(&tagged.tag, "a value", path));
            from_yaml(tagged.value, path, diagnostics, report)
        }
    }
}

/// The note an explicit tag is dropped with. `subject` names what carried it,
/// because a key and its value are one place in the block and two tags.
fn tag_stripped(tag: impl fmt::Display, subject: &str, path: &str) -> Diagnostic {
    Diagnostic::warning(
        DiagnosticCode::FrontmatterTagStripped,
        "an explicit YAML tag was dropped and what it tagged was kept; the value model \
         carries no tags",
    )
    .with_detail(format!("`{tag}` on {subject} {}", location(path)))
}

/// The account a repeated key is refused with, in the shape the parser behind
/// this seam states its own duplicate refusal in: the key, prefixed by the path
/// to the mapping holding it wherever that is not the top level.
///
/// The parser's own account also places the entry — `at line 2 column 3` — and
/// this one does not. A collapse is reached after the parse, which is where the
/// spans were, so what the two spellings of a repeated key share is the key and
/// the path and not a byte of position.
fn duplicate_entry(path: &str, key: &str) -> String {
    let entry = format!("duplicate entry with key {key:?}");
    if path.is_empty() {
        entry
    } else {
        format!("{path}: {entry}")
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
                DiagnosticCode::FrontmatterIntegerOutOfRange,
                "an integer outside the range of a 64-bit signed integer is carried as a float",
            )
            .with_detail(format!("`{value}` {}", location(path))),
        );
        return Value::Float(value as f64);
    }
    Value::Float(number.as_f64().unwrap_or(f64::NAN))
}

#[cfg(test)]
mod tests {
    use super::{HASH_ABOVE, KeyIndex, Mapping, Value};

    /// A mapping holding `filler` distinct keys and two entries under `dup`.
    ///
    /// A block whose keys repeat is refused before a mapping is carried, so
    /// this builds one past that refusal directly: what the cases below hold is
    /// that the two routes to a value agree whatever the entries are, not that
    /// these entries occur.
    fn with_a_repeated_key(filler: usize) -> Mapping {
        let mut map = Mapping::new();
        map.append("dup".to_string(), Value::Int(1));
        for index in 0..filler {
            map.append(format!("k{index}"), Value::Int(0));
        }
        map.append("dup".to_string(), Value::Int(2));
        map
    }

    /// A key written twice resolves to its first entry through a by-key view,
    /// which is the entry [`Mapping::get`] answers with — through the scanning
    /// arm and the hashing one alike.
    #[test]
    fn a_by_key_view_resolves_a_repeated_key_the_way_a_scan_does() {
        for filler in [0, HASH_ABOVE * 2] {
            let map = with_a_repeated_key(filler);
            let view = KeyIndex::of(&map);
            assert_eq!(view.get("dup"), map.get("dup"));
            assert_eq!(view.get("dup"), Some(&Value::Int(1)));
            assert!(view.contains_key("dup"));
            assert_eq!(view.get("absent"), None);
            assert!(!view.contains_key("absent"));
        }
    }

    /// **Both arms of the uniqueness check answer the same question.** The arm
    /// a mapping takes is a cost decision, so a repeated key is found through
    /// the scan a small mapping takes and through the table a large one takes
    /// alike, and a mapping whose keys are distinct answers nothing on either
    /// route. Nothing but this holds the hashing arm: a block of more keys than
    /// [`HASH_ABOVE`] is refused by it alone.
    #[test]
    fn the_uniqueness_check_finds_a_repeated_key_through_either_arm() {
        for filler in [0, HASH_ABOVE * 2] {
            let map = with_a_repeated_key(filler);
            assert_eq!(map.repeated_key(), Some("dup"), "{} entries", map.len());
        }
        for entries in [0, 1, HASH_ABOVE, HASH_ABOVE + 1, HASH_ABOVE * 2] {
            assert_eq!(of_size(entries).repeated_key(), None, "{entries} entries");
        }
    }

    /// A mapping holding `entries` distinct keys.
    fn of_size(entries: usize) -> Mapping {
        let mut map = Mapping::new();
        for index in 0..entries {
            map.append(format!("k{index}"), Value::Int(0));
        }
        map
    }

    /// Which arm a view takes turns at [`HASH_ABOVE`] exactly: a mapping of that
    /// many entries scans, and one entry more hashes. The boundary is what this
    /// holds, so the constant cannot move without a diff a reviewer reads.
    #[test]
    fn the_arm_a_view_takes_turns_at_hash_above() {
        let scanned = of_size(HASH_ABOVE);
        let hashed = of_size(HASH_ABOVE + 1);
        assert_eq!(scanned.len(), HASH_ABOVE);
        assert_eq!(hashed.len(), HASH_ABOVE + 1);
        assert!(matches!(KeyIndex::of(&scanned), KeyIndex::Scan(_)));
        assert!(matches!(KeyIndex::of(&hashed), KeyIndex::Hashed(_)));
    }

    /// The fillers the repeated-key case walks sit on either side of that
    /// boundary, so it covers both arms rather than one of them twice.
    #[test]
    fn a_small_mapping_scans_and_a_large_one_hashes() {
        let small = with_a_repeated_key(0);
        let large = with_a_repeated_key(HASH_ABOVE * 2);
        assert!(matches!(KeyIndex::of(&small), KeyIndex::Scan(_)));
        assert!(matches!(KeyIndex::of(&large), KeyIndex::Hashed(_)));
    }
}
