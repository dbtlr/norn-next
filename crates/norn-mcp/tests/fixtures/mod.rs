//! The vendored MCP schemas, and validation against one of their
//! definitions. Shared by every suite that checks an emitted message.
#![allow(dead_code)] // Each suite uses its own share of this module.

use serde_json::{Value, json};

/// A JSON Schema 2020-12 validator for `schema`.
pub fn validator(schema: &Value) -> jsonschema::Validator {
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(schema)
        .unwrap()
}

/// A validator for `definition` of `revision`'s vendored schema, its
/// definitions retained.
pub fn official(revision: &str, definition: &str) -> jsonschema::Validator {
    let mut schema: Value = serde_json::from_str(match revision {
        "2025-11-25" => include_str!("2025-11-25.json"),
        "2026-07-28" => include_str!("2026-07-28.json"),
        other => panic!("no vendored schema for {other}"),
    })
    .unwrap();
    schema["$ref"] = json!(format!("#/$defs/{definition}"));
    validator(&schema)
}

/// Assert that `message` is valid as `definition` of `revision`. Each
/// validator is built once per suite.
pub fn assert_valid(revision: &str, definition: &str, message: &Value) {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    type Cache = Mutex<HashMap<(String, String), Arc<jsonschema::Validator>>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let validator = {
        let mut cache = CACHE.get_or_init(Cache::default).lock().unwrap();
        Arc::clone(
            cache
                .entry((revision.to_string(), definition.to_string()))
                .or_insert_with(|| Arc::new(official(revision, definition))),
        )
    };
    if let Err(error) = validator.validate(message) {
        panic!("not a valid {revision} {definition}: {error}: {message}");
    }
}
