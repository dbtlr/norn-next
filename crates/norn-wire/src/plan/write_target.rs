//! Which documents a frontmatter operation writes.
//!
//! **A target is one path, or every document a predicate list matches.** The
//! four frontmatter kinds take either: `set --where` is a target on the
//! operation rather than a verb of its own. On the wire a target is exactly
//! one of two keys, `path` or `where`, sitting among the fields that name it,
//! so a frontmatter operation's `path` is the key every other kind names its
//! document by. Both keys written, or neither, is refused, and so is an empty
//! `where`: a conjunction of no predicates matches every document.
//!
//! **A predicate list is the shared grammar, and planning expands it.** A
//! `where` target is the conjunction a `find` filters by. The planner expands
//! it at planning into one operation per matched document, each with a path
//! target, so a resolved plan carries only path targets: a resolved plan still
//! carrying a `where` target is a fault in its shape, which
//! [`ResolvedPlan::unexpanded_targets`](crate::ResolvedPlan::unexpanded_targets)
//! names. The match set is not a condition — a resolved plan sent again writes
//! exactly the documents it was previewed with.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::document::DocumentPath;
use crate::plan::operation::written;
use crate::predicate::Predicate;

/// Which documents an operation writes: one path, or every document a
/// predicate list matches.
///
/// On the wire a target is exactly one of two keys among the fields that
/// name it: `"path":"notes/a.md"`, or
/// `"where":[{"op":"eq","key":"status","value":"draft"}]`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WriteTarget {
    /// The document at this path.
    Path(DocumentPath),
    /// Every document matching this conjunction, expanded at planning.
    Where(Vec<Predicate>),
}

impl WriteTarget {
    /// The document at `path`.
    pub const fn path(path: DocumentPath) -> Self {
        WriteTarget::Path(path)
    }

    /// Every document matching every one of `predicates`.
    pub fn matching(predicates: impl IntoIterator<Item = Predicate>) -> Self {
        WriteTarget::Where(predicates.into_iter().collect())
    }

    /// The path this target names, or `None` for a `where` target planning
    /// has not expanded.
    pub const fn as_path(&self) -> Option<&DocumentPath> {
        match self {
            WriteTarget::Path(path) => Some(path),
            WriteTarget::Where(_) => None,
        }
    }

    /// The target the two keys spell: exactly one of them written, and a
    /// `where` holding at least one predicate, since a conjunction of none
    /// matches every document.
    pub(crate) fn from_keys(
        path: Option<DocumentPath>,
        predicates: Option<Vec<Predicate>>,
    ) -> Result<Self, &'static str> {
        match (path, predicates) {
            (Some(path), None) => Ok(WriteTarget::Path(path)),
            (None, Some(predicates)) if predicates.is_empty() => {
                Err("a `where` target names at least one predicate")
            }
            (None, Some(predicates)) => Ok(WriteTarget::Where(predicates)),
            (Some(_), Some(_)) => Err("a target is `path` or `where`, not both"),
            (None, None) => Err("a target names `path` or `where`"),
        }
    }
}

impl Serialize for WriteTarget {
    /// A target is written as its one key, so a target flattened among an
    /// operation's fields is that key beside them.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut target = serializer.serialize_struct("WriteTarget", 1)?;
        match self {
            WriteTarget::Path(path) => target.serialize_field("path", path)?,
            WriteTarget::Where(predicates) => target.serialize_field("where", predicates)?,
        }
        target.end()
    }
}

/// A target as it arrives on its own: its two keys, each held as whether it
/// was written, and no other.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetKeys {
    #[serde(default, deserialize_with = "written")]
    path: Option<DocumentPath>,
    #[serde(default, rename = "where", deserialize_with = "written")]
    predicates: Option<Vec<Predicate>>,
}

impl<'de> Deserialize<'de> for WriteTarget {
    /// A target read on its own is an object holding exactly one of its two
    /// keys. Among an operation's fields, the operation's reader takes the
    /// two keys itself, by the same rule.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let TargetKeys { path, predicates } = TargetKeys::deserialize(deserializer)?;
        WriteTarget::from_keys(path, predicates).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for WriteTarget {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("WriteTarget")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::WriteTarget")
    }

    /// The two keys, exactly one of them required.
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        target_schema(generator)
    }
}

/// What a target's schema says it is.
const TARGET_DESCRIPTION: &str = "Which documents an operation writes: exactly one of `path`, the document at that path, or `where`, every document matching a conjunction of predicates, expanded at planning.";

/// The schema of a target: its two keys, and exactly one of them required.
fn target_schema(generator: &mut SchemaGenerator) -> Schema {
    let (path, predicates) = target_properties(generator);
    json_schema!({
        "description": TARGET_DESCRIPTION,
        "type": "object",
        "properties": {
            "path": path,
            "where": predicates,
        },
        "oneOf": [
            { "required": ["path"] },
            { "required": ["where"] },
        ],
        "additionalProperties": false,
    })
}

/// The derived schema of an object a target is flattened into, written the
/// way an object whose keys are all its own is.
///
/// The derive merges the target's two keys and its one-of-two rule into the
/// object, but carries the target's description onto an object that has none
/// of its own, and refuses an unknown key by `unevaluatedProperties`. The
/// object already lists every key it takes, so it refuses the rest by
/// `additionalProperties`, as every other object that refuses one does, and
/// the target's description is dropped from it.
pub(crate) fn settle_flattened_target(object: &mut Schema) {
    let Some(object) = object.as_object_mut() else {
        return;
    };
    if object.remove("unevaluatedProperties").is_some() {
        object.insert("additionalProperties".to_string(), false.into());
    }
    if object.get("description").and_then(|text| text.as_str()) == Some(TARGET_DESCRIPTION) {
        object.remove("description");
    }
}

/// The two target keys' schemas, each with its own description.
fn target_properties(generator: &mut SchemaGenerator) -> (Schema, Schema) {
    let mut path = generator.subschema_for::<DocumentPath>();
    path.insert(
        "description".to_string(),
        "The document written. Exactly one of `path` and `where` is written.".into(),
    );
    let mut predicates = generator.subschema_for::<Vec<Predicate>>();
    predicates.insert(
        "description".to_string(),
        "Every document matching all of these predicates is written; planning expands the match into one operation per document. At least one predicate is required. Exactly one of `path` and `where` is written."
            .into(),
    );
    predicates.insert("minItems".to_string(), 1.into());
    (path, predicates)
}
