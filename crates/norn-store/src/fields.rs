//! The field pillar's rows, derived from one document's frontmatter value.
//!
//! [`FieldRows::derive`] is the one place a document's field rows are made.
//! The host calls it where it plans a document, so it can hand the rows the
//! typed order its pinned schema declares; the increment calls it again with no
//! declaration to check that the rows it was handed are the rows the document's
//! own frontmatter derives. One function on both sides is what keeps the two
//! from being two readings of one value.
//!
//! # What a document's rows are
//!
//! Only a frontmatter value that is a map yields rows; any other value — and
//! no value — yields none. Per key, in key order:
//!
//! - **One presence row** carrying the container the key's value sits in —
//!   a scalar, a sequence or a map — and no value. An empty sequence and an
//!   empty map are present, and the container tells them apart.
//! - **One value row per scalar**, and one per scalar element of a sequence in
//!   element order. A map yields no value rows: its entries are read off the
//!   canonical projection, not queried. A sequence element that is itself a
//!   sequence or a map yields no value row either.
//!
//! A repeated key keeps its last value, as the canonical projection does.
//!
//! # The raw text is the scalar's canonical text, unquoted
//!
//! `true` and `false`, an integer's digits, a float in the spelling the
//! canonical projection writes, and a string's own content without quotes —
//! because a request's value and a projected field's value both cross as that
//! unquoted text, and a quoted spelling here would be an equality that never
//! matches. A null, and a float JSON cannot spell, is a scalar whose raw text is
//! SQL `NULL`: the key is still present, and no equality matches it.
//!
//! # The typed value, and the least-value markers
//!
//! Beside the raw text a value row carries a **typed** sort key where the
//! declaration hands [`DeclaredFields::typed_order`] one for the key and the raw
//! text reads as that type; everything else carries none. The key's bytewise
//! order is the declared type's order, so a typed sort is an order over text.
//!
//! Each key's **least value** is marked twice, once under the raw order and once
//! under the typed order, because the two can pick different rows: `"10"` is
//! the least raw text of `["9", "10"]` and nine the least number. A tie goes to
//! the earliest element. The markers are computed here, with the rows, before
//! anything is written, so a sort over a set-valued field can read one row per
//! document — its least value — rather than every value it holds.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use crate::json::{FrontmatterValue, float_text};

/// The container a key's value sits in, as its presence row records it.
///
/// The store's own reading of the column, as [`crate::TagSource`] is: the
/// statement writes [`FieldContainer::as_str`], and the reader parses it back.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldContainer {
    /// One value: a string, a number, a boolean or a null.
    Scalar,
    /// A sequence of values.
    Sequence,
    /// A mapping of values by key.
    Map,
}

impl FieldContainer {
    /// Every container, in the order the column's `CHECK` lists them.
    pub const ALL: [FieldContainer; 3] = [
        FieldContainer::Scalar,
        FieldContainer::Sequence,
        FieldContainer::Map,
    ];

    /// The container as the column holds it.
    pub const fn as_str(self) -> &'static str {
        match self {
            FieldContainer::Scalar => "scalar",
            FieldContainer::Sequence => "sequence",
            FieldContainer::Map => "map",
        }
    }

    /// The container a column value names, or nothing.
    pub(crate) fn parse(written: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|container| container.as_str() == written)
    }
}

/// One row of a document's field pillar.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FieldRow {
    /// The document carries `key`, and its value sits in `container`. It is
    /// ordinal zero under its key.
    Presence {
        key: String,
        container: FieldContainer,
    },
    /// One scalar the document carries under `key`.
    Value {
        key: String,
        /// Where the scalar stands among the key's values, from one, in
        /// element order.
        ordinal: u32,
        /// The canonical text, or `None` for a null and a float JSON cannot
        /// spell.
        raw: Option<String>,
        /// The declared type's sort key, or `None` where the key carries no
        /// typed order or the raw text does not read as the declared type.
        typed: Option<String>,
        /// Whether this is the key's least value under the raw order.
        least_raw: bool,
        /// Whether this is the key's least value under the typed order.
        least_typed: bool,
    },
}

impl FieldRow {
    /// The key this row is about.
    pub fn key(&self) -> &str {
        match self {
            FieldRow::Presence { key, .. } | FieldRow::Value { key, .. } => key,
        }
    }

    /// Where the row stands under its key: zero for the presence row, and the
    /// value's place from one.
    pub fn ordinal(&self) -> u32 {
        match self {
            FieldRow::Presence { .. } => 0,
            FieldRow::Value { ordinal, .. } => *ordinal,
        }
    }

    /// The same row with its typed half removed, which is the half the store
    /// cannot derive without a declaration.
    fn untyped(&self) -> FieldRow {
        match self {
            FieldRow::Presence { .. } => self.clone(),
            FieldRow::Value {
                key,
                ordinal,
                raw,
                least_raw,
                ..
            } => FieldRow::Value {
                key: key.clone(),
                ordinal: *ordinal,
                raw: raw.clone(),
                typed: None,
                least_raw: *least_raw,
                least_typed: false,
            },
        }
    }
}

/// Every field row one document derives, in key order and, under a key, in
/// ordinal order.
///
/// Only [`FieldRows::derive`] makes one, so the rows, the order they stand in
/// and the least-value markers are always the ones a frontmatter value
/// derives: what a caller can choose is which value and which declaration.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FieldRows {
    rows: Vec<FieldRow>,
}

impl FieldRows {
    /// The rows `frontmatter` derives, with the typed values `declared` gives
    /// them.
    pub fn derive(frontmatter: Option<&FrontmatterValue>, declared: &DeclaredFields) -> Self {
        let Some(FrontmatterValue::Map(entries)) = frontmatter else {
            return FieldRows::default();
        };
        // The last write of a repeated key is the one that stands, which is
        // the value the canonical projection keeps.
        let standing: BTreeMap<&str, &FrontmatterValue> = entries
            .iter()
            .map(|(key, value)| (key.as_str(), value))
            .collect();
        let mut rows = Vec::new();
        for (key, value) in standing {
            let (container, scalars) = match value {
                FrontmatterValue::Sequence(items) => (
                    FieldContainer::Sequence,
                    items.iter().filter_map(scalar_text).collect(),
                ),
                FrontmatterValue::Map(_) => (FieldContainer::Map, Vec::new()),
                scalar => (
                    FieldContainer::Scalar,
                    scalar_text(scalar).into_iter().collect(),
                ),
            };
            rows.push(FieldRow::Presence {
                key: key.to_string(),
                container,
            });
            let order = declared.typed_order(key);
            let typed: Vec<Option<String>> = scalars
                .iter()
                .map(|raw| {
                    order.and_then(|order| raw.as_deref().and_then(|raw| order.sort_key(raw)))
                })
                .collect();
            let least_raw = least(&scalars);
            let least_typed = least(&typed);
            for (index, (raw, typed)) in scalars.into_iter().zip(typed).enumerate() {
                rows.push(FieldRow::Value {
                    key: key.to_string(),
                    ordinal: index as u32 + 1,
                    raw,
                    typed,
                    least_raw: least_raw == Some(index),
                    least_typed: least_typed == Some(index),
                });
            }
        }
        FieldRows { rows }
    }

    /// Rows read back from the store, which the write put there in this order.
    pub(crate) fn stored(rows: Vec<FieldRow>) -> Self {
        FieldRows { rows }
    }

    /// The rows, in key order and then ordinal order.
    pub fn rows(&self) -> &[FieldRow] {
        &self.rows
    }

    /// Whether the document derives no field rows at all.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Whether these rows and `other` are the same rows once the typed half of
    /// each is set aside.
    pub(crate) fn agree_untyped(&self, other: &FieldRows) -> bool {
        self.rows.len() == other.rows.len()
            && self
                .rows
                .iter()
                .zip(&other.rows)
                .all(|(mine, theirs)| mine.untyped() == theirs.untyped())
    }
}

/// A scalar's raw text, or nothing where the value is a container.
///
/// The outer `Option` says whether the value is a scalar at all; the inner one
/// is the raw text, which a null and a non-finite float do not have.
fn scalar_text(value: &FrontmatterValue) -> Option<Option<String>> {
    match value {
        FrontmatterValue::Null => Some(None),
        FrontmatterValue::Bool(flag) => Some(Some(flag.to_string())),
        FrontmatterValue::Int(number) => Some(Some(number.to_string())),
        FrontmatterValue::Float(number) => Some(float_text(*number)),
        FrontmatterValue::String(text) => Some(Some(text.clone())),
        FrontmatterValue::Sequence(_) | FrontmatterValue::Map(_) => None,
    }
}

/// The position of the least present value, the earliest one on a tie.
fn least(values: &[Option<String>]) -> Option<usize> {
    values
        .iter()
        .enumerate()
        .filter_map(|(index, value)| value.as_deref().map(|value| (value, index)))
        .min()
        .map(|(_, index)| index)
}

/// The fields a vault schema declares, and the typed order each typed one
/// carries.
///
/// The store reads no schema: the host derives this value from the schema it
/// pinned and hands it over, and [`FieldRows::derive`] reads it to fill the
/// typed column. A key declared without a typed order is ordered by its raw
/// text, which is what a field declared as text is.
#[derive(Clone, Debug, Default)]
pub struct DeclaredFields {
    keys: BTreeMap<String, Option<TypedOrder>>,
}

impl DeclaredFields {
    /// A vault that declares no field.
    pub fn none() -> Self {
        Self::default()
    }

    /// The same declaration with `key` declared and ordered by its raw text.
    pub fn declare(mut self, key: impl Into<String>) -> Self {
        self.keys.insert(key.into(), None);
        self
    }

    /// The same declaration with `key` declared and ordered by `order`.
    pub fn declare_typed(mut self, key: impl Into<String>, order: TypedOrder) -> Self {
        self.keys.insert(key.into(), Some(order));
        self
    }

    /// Whether the schema declares `key`.
    pub fn is_declared(&self, key: &str) -> bool {
        self.keys.contains_key(key)
    }

    /// The typed order `key` carries, where it is declared with one.
    pub fn typed_order(&self, key: &str) -> Option<&TypedOrder> {
        self.keys.get(key).and_then(Option::as_ref)
    }

    /// The declared keys, in key order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.keys.keys().map(String::as_str)
    }
}

/// How one declared type reads a raw value into the key it sorts by.
///
/// It maps a raw text to a sort key whose bytewise order is the type's order,
/// or to nothing where the text does not read as the type. The host builds one
/// from the schema's type; the store only applies it.
#[derive(Clone)]
pub struct TypedOrder(Arc<SortKeyOf>);

/// The function a [`TypedOrder`] applies: raw text in, sort key out.
type SortKeyOf = dyn Fn(&str) -> Option<String> + Send + Sync;

impl TypedOrder {
    /// The order `sort_key` computes.
    pub fn new(sort_key: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> Self {
        TypedOrder(Arc::new(sort_key))
    }

    /// The sort key `raw` reads as, or nothing where it does not read as this
    /// type.
    pub fn sort_key(&self, raw: &str) -> Option<String> {
        (self.0)(raw)
    }
}

impl fmt::Debug for TypedOrder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TypedOrder(..)")
    }
}
