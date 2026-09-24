//! The field pillar's rows, derived from one document's frontmatter value.
//!
//! [`FieldRows::derive`] is the one place a document's field rows are made.
//! [`crate::DocumentFacts::with_frontmatter`] calls it where the host plans a
//! document, under the typed order its pinned schema declares, and is the one
//! way a document's frontmatter and rows are set: both are private to the
//! facts, so the rows a document carries are the rows its own frontmatter
//! derives, and the increment writes them as handed.
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
//! declaration hands [`ContentModel::typed_order`] one for the key and the raw
//! text reads as that type; everything else carries none. The key's bytewise
//! order is the declared type's order, so a typed sort is an order over text.
//!
//! Each key's **least value** is marked twice, once under the raw order and once
//! under the typed order, because the two can pick different rows: `"10"` is
//! the least raw text of `["9", "10"]` and nine the least number. A tie goes to
//! the earliest element. The markers are computed here, with the rows, before
//! anything is written, so a sort over a set-valued field can read one row per
//! document — its least value — rather than every value it holds.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::Bound;
use std::sync::Arc;

use norn_wire::{Facet, FacetKind, FieldType, PathRuleKind, Pattern, TagStance};

use crate::json::{FrontmatterValue, float_text};
use crate::resolve::AmbiguityIgnore;

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
    pub fn derive(frontmatter: Option<&FrontmatterValue>, declared: &ContentModel) -> Self {
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

/// What a vault schema declares, as the store reads it: the declared fields,
/// the typed order each typed one carries, the places it keeps out of
/// ambiguity classes, the rest of the content model `describe` reports, and
/// the fingerprint of the schema it was all read from.
///
/// The store reads no schema: the host derives this value from the schema it
/// pinned and hands it over. [`FieldRows::derive`] reads the typed orders to
/// fill the typed column, a read compiles its keys under them, and `describe`
/// reports every declaration here as a facet — the declared fields with their
/// type, whether they are required and their closed set of values, the
/// declared tags, the tag patterns, the stance on an undeclared tag, the
/// declared folders and the path rules. A key declared without a typed order
/// is ordered by its raw text, which is what a field declared as text is.
///
/// **The ambiguity-ignore set is held once.** The one path rule a schema
/// states is ambiguity-ignore, and its globs are held as the
/// [`AmbiguityIgnore`] set the resolver applies to every class a target opens;
/// `describe` reports each glob of that same set as a path rule, so what a
/// resolution ignores and what `describe` says it ignores cannot disagree.
///
/// **The vocabulary a declaration is spelled in is `norn-wire`'s**: the type a
/// field is declared as, the stance on an undeclared tag and the rule a path
/// rule states are the enums a facet reports, so the store carries them rather
/// than defining a third spelling of each.
///
/// **The declaration names the schema it came from.** [`ContentModel::none`]
/// is the declaration of a store with no schema pinned, which declares nothing;
/// everything declared is declared [`ContentModel::under`] a schema
/// fingerprint. A typed value is therefore always derived under a named
/// schema, and the store compares that name with the one it pins: an
/// increment refuses typed rows derived under another, and a read refuses a
/// declaration that is not the snapshot's.
///
/// **Every declaration is held once, by the text that names it**: a field by
/// its key, a tag by its name, a tag pattern and a path rule by the pattern, a
/// folder by its path. A schema names each field and each folder once — its
/// grammar refuses a repeated field key and a folder path written twice — so
/// the host hands neither twice. A tag, a tag pattern or a path rule written
/// twice is the same text twice and carries nothing beyond it, so the two
/// collapse to one and nothing is lost.
#[derive(Clone, Debug, Default)]
pub struct ContentModel {
    schema: Option<String>,
    keys: BTreeMap<String, FieldDeclaration>,
    tags: BTreeSet<String>,
    tag_patterns: BTreeSet<String>,
    undeclared_tags: Option<TagStance>,
    folders: BTreeMap<String, Option<String>>,
    ambiguity_ignore: AmbiguityIgnore,
}

impl ContentModel {
    /// The declaration of a store with no schema pinned: no schema, and no
    /// declaration.
    pub fn none() -> Self {
        Self::default()
    }

    /// A declaration read from the schema pinned under `fingerprint`, declaring
    /// nothing yet.
    pub fn under(fingerprint: impl Into<String>) -> Self {
        ContentModel {
            schema: Some(fingerprint.into()),
            ..Self::default()
        }
    }

    /// The same declaration with `key` declared as text: not required, not
    /// closed, and ordered by its raw text.
    ///
    /// # Panics
    ///
    /// On a declaration with no schema: [`ContentModel::none`] declares
    /// nothing, and everything is declared [`ContentModel::under`] the
    /// schema that declares it. Every method that declares something panics
    /// alike.
    pub fn declare(self, key: impl Into<String>) -> Self {
        self.declare_field(key, FieldDeclaration::text())
    }

    /// The same declaration with `key` declared as `declaration`, ordered by
    /// the typed order its type carries, or by its raw text where it carries
    /// none.
    pub fn declare_field(mut self, key: impl Into<String>, declaration: FieldDeclaration) -> Self {
        let key = key.into();
        self.schema_declares(&key);
        self.keys.insert(key, declaration);
        self
    }

    /// The same declaration with the tag `name` declared. A name declared
    /// again is the declaration already held.
    pub fn declare_tag(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        self.schema_declares(&name);
        self.tags.insert(name);
        self
    }

    /// The same declaration with the tag facet admitting `pattern` beyond its
    /// literal names. A pattern declared again is the declaration already
    /// held.
    pub fn declare_tag_pattern(mut self, pattern: impl Into<String>) -> Self {
        let pattern = pattern.into();
        self.schema_declares(&pattern);
        self.tag_patterns.insert(pattern);
        self
    }

    /// The same declaration with `stance` as what the schema says about a tag
    /// its facet does not admit.
    pub fn declare_undeclared_tags(mut self, stance: TagStance) -> Self {
        self.schema_declares(stance.as_str());
        self.undeclared_tags = Some(stance);
        self
    }

    /// The same declaration with the folder at `path` declared, for what
    /// `description` says. A schema's grammar refuses a folder path written
    /// twice, so the host declares each path once.
    pub fn declare_folder(mut self, path: impl Into<String>, description: Option<String>) -> Self {
        let path = path.into();
        self.schema_declares(&path);
        self.folders.insert(path, description);
        self
    }

    /// The same declaration with the places `pattern` names kept out of every
    /// ambiguity class a resolution reads under it: the ambiguity-ignore path
    /// rule, stated over `pattern`. A glob declared again is the declaration
    /// already held.
    pub fn declare_ambiguity_ignore(mut self, pattern: Pattern) -> Self {
        self.schema_declares(pattern.as_str());
        self.ambiguity_ignore = self.ambiguity_ignore.with(pattern);
        self
    }

    fn schema_declares(&self, named: &str) {
        assert!(
            self.schema.is_some(),
            "`{named}` is declared on a declaration no schema makes"
        );
    }

    /// The fingerprint of the schema this declaration was read from, or `None`
    /// for the declaration of a store with no schema pinned.
    pub fn schema(&self) -> Option<&str> {
        self.schema.as_deref()
    }

    /// Whether the schema declares `key`.
    pub fn is_declared(&self, key: &str) -> bool {
        self.keys.contains_key(key)
    }

    /// The typed order `key` carries, where it is declared with one.
    pub fn typed_order(&self, key: &str) -> Option<&TypedOrder> {
        self.keys
            .get(key)
            .and_then(|declaration| declaration.order.as_ref())
    }

    /// The places the schema keeps out of ambiguity classes.
    pub fn ambiguity_ignore(&self) -> &AmbiguityIgnore {
        &self.ambiguity_ignore
    }

    /// The declared keys, in key order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.keys.keys().map(String::as_str)
    }

    /// Every facet of `kind` this declaration reports keyed after `after` —
    /// from the first where it is `None` — in the order of the text that keys
    /// it: the byte order of a field's key, a tag's name, a pattern, a
    /// folder's path or the stance's spelling. Each is reported once.
    ///
    /// A pure read of the declaration: it runs no statement, and it builds a
    /// facet only as the iterator is drawn, so a page drawing the facets it
    /// returns builds no others. Observed fields are what documents carry
    /// rather than what the schema declares, so this reports none; and a
    /// declaration no schema makes reports nothing of any kind.
    pub fn facets_of<'a>(
        &'a self,
        kind: FacetKind,
        after: Option<&'a str>,
    ) -> Box<dyn Iterator<Item = Facet> + 'a> {
        match kind {
            FacetKind::DeclaredField => {
                Box::new(keyed_after(&self.keys, after).map(|(key, declaration)| {
                    Facet::declared_field(
                        key.clone(),
                        declaration.field_type,
                        declaration.required,
                        declaration.one_of.clone(),
                    )
                }))
            }
            FacetKind::DeclaredTag => {
                Box::new(named_after(&self.tags, after).map(Facet::declared_tag))
            }
            FacetKind::TagPattern => {
                Box::new(named_after(&self.tag_patterns, after).map(Facet::tag_pattern))
            }
            FacetKind::Folder => Box::new(
                keyed_after(&self.folders, after)
                    .map(|(path, description)| Facet::folder(path.clone(), description.clone())),
            ),
            FacetKind::PathRule => {
                Box::new(self.ambiguity_ignore.after(after).map(|pattern| {
                    Facet::path_rule(PathRuleKind::AmbiguityIgnore, pattern.as_str())
                }))
            }
            FacetKind::UndeclaredTags => Box::new(
                self.undeclared_tags
                    .filter(|stance| after.is_none_or(|after| stance.as_str() > after))
                    .into_iter()
                    .map(Facet::undeclared_tags),
            ),
            // Observed fields, and a kind this build does not know, are
            // declared nowhere.
            _ => Box::new(std::iter::empty()),
        }
    }
}

/// The entries of `map` keyed after `after`, or every entry where it is
/// `None`, in key order.
fn keyed_after<'a, V>(
    map: &'a BTreeMap<String, V>,
    after: Option<&'a str>,
) -> impl Iterator<Item = (&'a String, &'a V)> {
    map.range::<str, _>((
        after.map_or(Bound::Unbounded, Bound::Excluded),
        Bound::Unbounded,
    ))
}

/// The names in `set` after `after`, or every name where it is `None`, in
/// name order.
fn named_after<'a>(
    set: &'a BTreeSet<String>,
    after: Option<&'a str>,
) -> impl Iterator<Item = &'a String> {
    set.range::<str, _>((
        after.map_or(Bound::Unbounded, Bound::Excluded),
        Bound::Unbounded,
    ))
}

/// What a schema declares one field as: its type, the typed order that type
/// reads a raw value into where it does not order as text, whether every
/// document is declared to carry it, and the closed set of values it is
/// declared to hold.
///
/// **A type and its order are made together.** There is one constructor per
/// type: `text` and `tags` order by their raw text and take no order, and
/// `number`, `boolean` and `date` each take the [`TypedOrder`] their type reads
/// a raw value into, so a declaration whose type and order disagree on whether
/// it is typed has no spelling. What an order computes is the host's: it builds
/// each one from the schema's own reading of the type.
#[derive(Clone, Debug)]
pub struct FieldDeclaration {
    field_type: FieldType,
    order: Option<TypedOrder>,
    required: bool,
    one_of: Option<Vec<String>>,
}

impl FieldDeclaration {
    /// A field declared as `field_type` and ordered by `order`: not required,
    /// and not closed.
    const fn of(field_type: FieldType, order: Option<TypedOrder>) -> Self {
        FieldDeclaration {
            field_type,
            order,
            required: false,
            one_of: None,
        }
    }

    /// A field declared as text, ordered by its raw text.
    pub const fn text() -> Self {
        Self::of(FieldType::Text, None)
    }

    /// A field declared as a set of tag names, ordered by their raw text.
    pub const fn tags() -> Self {
        Self::of(FieldType::Tags, None)
    }

    /// A field declared as a number, ordered by `order`.
    pub const fn number(order: TypedOrder) -> Self {
        Self::of(FieldType::Number, Some(order))
    }

    /// A field declared as a boolean, ordered by `order`.
    pub const fn boolean(order: TypedOrder) -> Self {
        Self::of(FieldType::Boolean, Some(order))
    }

    /// A field declared as a date, ordered by `order`.
    pub const fn date(order: TypedOrder) -> Self {
        Self::of(FieldType::Date, Some(order))
    }

    /// The same declaration, with every document declared to carry the field.
    #[must_use]
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    /// The same declaration, closed over `values`.
    #[must_use]
    pub fn one_of(mut self, values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.one_of = Some(values.into_iter().map(Into::into).collect());
        self
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
