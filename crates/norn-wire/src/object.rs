//! The one reader of a JSON object that takes nothing but an object.
//!
//! A derived reader of a struct also takes its values as an array, in field
//! order, and a reader into a `serde_json::Map` takes a key given twice as
//! whichever came last. Neither is the shape the frozen exchanges spell, so
//! their hand-written readers read through this: an object, each key once.

use std::fmt;

use serde::Deserializer;
use serde::de::{Error as _, MapAccess, Visitor};
use serde_json::{Map, Value};

/// The object `deserializer` holds, or a refusal when it holds anything else
/// or names a key twice.
pub(crate) fn read_object<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Map<String, Value>, D::Error> {
    deserializer.deserialize_map(ObjectVisitor)
}

struct ObjectVisitor;

impl<'de> Visitor<'de> for ObjectVisitor {
    type Value = Map<String, Value>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
        let mut object = Map::new();
        while let Some(key) = access.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(A::Error::custom(format!("duplicate field `{key}`")));
            }
            let value = access.next_value::<Value>()?;
            object.insert(key, value);
        }
        Ok(object)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(json: &str) -> Result<Map<String, Value>, serde_json::Error> {
        read_object(&mut serde_json::Deserializer::from_str(json))
    }

    #[test]
    fn an_object_is_read_and_anything_else_is_refused() {
        assert_eq!(read(r#"{"a":1,"b":[2]}"#).expect("an object").len(), 2);
        assert!(read("{}").expect("an empty object").is_empty());
        for not_an_object in ["[]", "[1, true]", "null", "1", "\"{}\""] {
            assert!(
                read(not_an_object).is_err(),
                "`{not_an_object}` read as an object"
            );
        }
    }

    #[test]
    fn a_key_given_twice_is_refused() {
        let error = read(r#"{"a":1,"a":2}"#).expect_err("a key given twice");
        assert!(error.to_string().contains("duplicate field `a`"), "{error}");
    }
}
