//! The value an author writes into a frontmatter field.
//!
//! **A written value is typed, where a read value is text.** A read answers
//! with [`FieldValue`](crate::FieldValue): the text each leaf is written as,
//! because what that text means is the vault schema's to decide behind the
//! store. A write goes the other way: the author says exactly what the field
//! is to hold — null, a boolean, an integer, a finite number, a string, a list
//! or an ordered map — and the planner writes exactly that, converted one to
//! one into the text layer's value, never coerced by the schema. A value that
//! does not fit the schema is refused by the introduced-violation check, not
//! rewritten. Coercing a string a person typed into a typed value is a
//! surface's, against the field types `describe` reports.
//!
//! **A written value is the plain JSON value of its shape.** `null`, `true`,
//! `3`, `2.5`, `"done"`, `["a","b"]`, `{"k":1}`: there is no tag, because a
//! value's shape is already its kind, and an author writing a field writes it
//! the way the field reads in a document. An integer is a number written with
//! no fraction and no exponent that fits a signed 64-bit integer. A `u64`
//! above `i64::MAX` is refused, since no float holds it exactly. Any other
//! number is a float: what the format delivers as a float, including an
//! integer beyond `u64` or below `i64::MIN`, which reads as a float that no
//! longer holds it exactly. A float is finite — a format that can spell `NaN`
//! or an infinity has that value refused, since no frontmatter field can hold
//! it. A map keeps the order its keys are written in, which is the order the
//! document writes them, and refuses a key written twice rather than keeping
//! either.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A number with a fraction that is finite: never `NaN` and never an
/// infinity.
///
/// On the wire a float is the number itself: `2.5`.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct FiniteFloat(f64);

impl FiniteFloat {
    /// `number` as a finite float, or the reason it is none.
    pub fn new(number: f64) -> Result<Self, NonFiniteFloat> {
        if number.is_finite() {
            Ok(FiniteFloat(number))
        } else {
            Err(NonFiniteFloat)
        }
    }

    /// The float as the number it is, which is finite by construction.
    pub const fn get(self) -> f64 {
        self.0
    }
}

// A finite float's equality is reflexive — the one value that breaks `f64`'s,
// `NaN`, cannot be built — so equality is an equivalence here.
impl Eq for FiniteFloat {}

impl fmt::Display for FiniteFloat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl Serialize for FiniteFloat {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f64(self.0)
    }
}

impl<'de> Deserialize<'de> for FiniteFloat {
    /// A float arrives as a number and is read through the constructor, so
    /// `NaN` and an infinity are refused.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let number = f64::deserialize(deserializer)?;
        FiniteFloat::new(number).map_err(de::Error::custom)
    }
}

impl JsonSchema for FiniteFloat {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("FiniteFloat")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::FiniteFloat")
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "number",
            "description": "A number with a fraction that is finite: never NaN and never an infinity.",
        })
    }
}

/// A number that is `NaN` or an infinity, which no written value holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NonFiniteFloat;

impl fmt::Display for NonFiniteFloat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a written float is finite: never NaN and never an infinity")
    }
}

impl std::error::Error for NonFiniteFloat {}

/// The entries of a written map, in the order they are written, each key
/// once.
///
/// On the wire a map is the JSON object itself: `{"owner":"drew","rank":2}`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ValueMap(Vec<(String, AuthoredValue)>);

impl ValueMap {
    /// The map holding `entries` in the order given, or the key written
    /// twice.
    pub fn new(
        entries: impl IntoIterator<Item = (String, AuthoredValue)>,
    ) -> Result<Self, DuplicateKey> {
        let mut map = ValueMap(Vec::new());
        for (key, value) in entries {
            map.insert(key, value)?;
        }
        Ok(map)
    }

    /// Append `value` under `key`, refusing a key the map already holds.
    fn insert(&mut self, key: String, value: AuthoredValue) -> Result<(), DuplicateKey> {
        if self.0.iter().any(|(held, _)| *held == key) {
            return Err(DuplicateKey(key));
        }
        self.0.push((key, value));
        Ok(())
    }

    /// The entries, in the order they are written.
    pub fn entries(&self) -> &[(String, AuthoredValue)] {
        &self.0
    }

    /// The entries, in the order they are written, taken out of the map.
    pub fn into_entries(self) -> Vec<(String, AuthoredValue)> {
        self.0
    }
}

/// A key a written map holds twice, which it refuses rather than keeping
/// either value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DuplicateKey(pub String);

impl fmt::Display for DuplicateKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "a written map holds each key once, and `{}` is written twice",
            self.0
        )
    }
}

impl std::error::Error for DuplicateKey {}

/// What an author writes into a frontmatter field: exactly the value the
/// field is to hold.
///
/// On the wire a value is the plain JSON value of its shape: `null`, `true`,
/// `3`, `2.5`, `"done"`, `["a","b"]`, `{"owner":"drew"}`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthoredValue {
    /// Null.
    Null,
    /// A boolean.
    Bool(bool),
    /// An integer, written with no fraction and no exponent.
    Integer(i64),
    /// A finite number with a fraction.
    Float(FiniteFloat),
    /// A string.
    String(String),
    /// A list of values, in order.
    List(Vec<AuthoredValue>),
    /// A map of values by string key, in the order they are written.
    Map(ValueMap),
}

impl AuthoredValue {
    /// The string `text`.
    pub fn string(text: impl Into<String>) -> Self {
        AuthoredValue::String(text.into())
    }

    /// The finite float `number`, or the reason it is none.
    pub fn float(number: f64) -> Result<Self, NonFiniteFloat> {
        FiniteFloat::new(number).map(AuthoredValue::Float)
    }

    /// The list of `items`.
    pub fn list(items: impl IntoIterator<Item = AuthoredValue>) -> Self {
        AuthoredValue::List(items.into_iter().collect())
    }

    /// The map of `entries` in the order given, or the key written twice.
    pub fn map(
        entries: impl IntoIterator<Item = (String, AuthoredValue)>,
    ) -> Result<Self, DuplicateKey> {
        ValueMap::new(entries).map(AuthoredValue::Map)
    }
}

impl Serialize for AuthoredValue {
    /// A value is written as the plain value of its shape.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            AuthoredValue::Null => serializer.serialize_unit(),
            AuthoredValue::Bool(value) => serializer.serialize_bool(*value),
            AuthoredValue::Integer(value) => serializer.serialize_i64(*value),
            AuthoredValue::Float(value) => serializer.serialize_f64(value.get()),
            AuthoredValue::String(value) => serializer.serialize_str(value),
            AuthoredValue::List(items) => {
                let mut sequence = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    sequence.serialize_element(item)?;
                }
                sequence.end()
            }
            AuthoredValue::Map(map) => map.serialize(serializer),
        }
    }
}

impl Serialize for ValueMap {
    /// A map is written as an object, its keys in the order they are held.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

/// Reads a written value from whatever shape the format hands over.
struct AuthoredValueVisitor;

impl<'de> Visitor<'de> for AuthoredValueVisitor {
    type Value = AuthoredValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .write_str("null, a boolean, an integer, a finite number, a string, a list or a map")
    }

    fn visit_unit<E: de::Error>(self) -> Result<AuthoredValue, E> {
        Ok(AuthoredValue::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<AuthoredValue, E> {
        Ok(AuthoredValue::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<AuthoredValue, D::Error>
    where
        D: Deserializer<'de>,
    {
        AuthoredValue::deserialize(deserializer)
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<AuthoredValue, E> {
        Ok(AuthoredValue::Bool(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<AuthoredValue, E> {
        Ok(AuthoredValue::Integer(value))
    }

    /// A `u64` past the signed 64-bit range is refused rather than read as a
    /// float that no longer holds it exactly.
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<AuthoredValue, E> {
        i64::try_from(value)
            .map(AuthoredValue::Integer)
            .map_err(|_| {
                E::custom(format_args!(
                    "a written integer fits a signed 64-bit integer, and {value} does not"
                ))
            })
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<AuthoredValue, E> {
        FiniteFloat::new(value)
            .map(AuthoredValue::Float)
            .map_err(E::custom)
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<AuthoredValue, E> {
        Ok(AuthoredValue::string(value))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<AuthoredValue, E> {
        Ok(AuthoredValue::String(value))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<AuthoredValue, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut items = Vec::new();
        while let Some(item) = sequence.next_element()? {
            items.push(item);
        }
        Ok(AuthoredValue::List(items))
    }

    /// Each key is read as a string and held in the order written; a key
    /// written twice is refused.
    fn visit_map<A>(self, mut entries: A) -> Result<AuthoredValue, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut map = ValueMap::default();
        while let Some(key) = entries.next_key::<String>()? {
            let value = entries.next_value()?;
            map.insert(key, value).map_err(de::Error::custom)?;
        }
        Ok(AuthoredValue::Map(map))
    }
}

impl<'de> Deserialize<'de> for AuthoredValue {
    /// A value is read from the shape the format hands over, through the
    /// grammar the constructors hold: a non-finite float and a map key
    /// written twice are refused.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(AuthoredValueVisitor)
    }
}

impl<'de> Deserialize<'de> for ValueMap {
    /// A map is read as a written value that must be a map.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match AuthoredValue::deserialize(deserializer)? {
            AuthoredValue::Map(map) => Ok(map),
            _ => Err(de::Error::custom("a written map is an object")),
        }
    }
}

impl JsonSchema for AuthoredValue {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("AuthoredValue")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::AuthoredValue")
    }

    /// Any plain JSON value, whose list items and map entries are values of
    /// this same type. JSON Schema has no way to say "finite" or "each key
    /// once", so the description says it.
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let value = generator.subschema_for::<AuthoredValue>();
        json_schema!({
            "description": "What an author writes into a frontmatter field: exactly the value the field is to hold, as the plain value of its shape — null, a boolean, an integer, a finite number, a string, a list or a map. A map keeps the order its keys are written in and holds each key once.",
            "type": ["null", "boolean", "integer", "number", "string", "array", "object"],
            "items": value,
            "additionalProperties": value,
        })
    }
}

impl JsonSchema for ValueMap {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("ValueMap")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::ValueMap")
    }

    /// An object whose entries are written values.
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let value = generator.subschema_for::<AuthoredValue>();
        json_schema!({
            "description": "A map of written values by string key, in the order they are written, each key once.",
            "type": "object",
            "additionalProperties": value,
        })
    }
}
