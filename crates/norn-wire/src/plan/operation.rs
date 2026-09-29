//! What a caller authors a plan in: operations, each a kind and its fields.
//!
//! **The kinds are a closed, typed list.** An operation's fields are the
//! fields its kind names and no others, typed as the rest of the vocabulary
//! types them: there is no untyped bag of fields a planner would have to read
//! a second time. A kind the list does not hold is refused at the read.
//!
//! **An operation is read by hand, because the derive cannot refuse what it
//! must.** On the wire an operation is `kind` and `fields` beside its own
//! optional parts, all in one object. A derive would spell the kind and its
//! fields as an enum flattened into the operation, and a flattened enum drops
//! a key it does not know where the operation must refuse one, while
//! `deny_unknown_fields` does not compose with flattening at all. So the
//! object is read by the derive into a private shape that refuses unknown
//! keys, whose `fields` is one private shape holding every field any kind
//! names; the kind then takes the fields it names, and refuses one it lacks
//! and one it does not take. Nothing is buffered: the derive reads every key
//! once, in whatever order it is written, so an object whose `fields` precedes
//! its `kind` reads, a key written twice is refused at every level, and a plan
//! read in any format reads the same inside a request as alone.
//!
//! **An edit's anchor is not a condition.** The text a `str_replace` replaces
//! is part of the operation: an operation whose anchor is gone no longer
//! resolves, which is a different outcome from a condition that fails.

use std::borrow::Cow;
use std::fmt;

use schemars::transform::transform_subschemas;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::document::DocumentPath;
use crate::plan::hash::ContentHash;

/// The identifier an operation is required by: a string naming something
/// rather than nothing.
///
/// On the wire an identifier is the string itself: `"move-a"`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct OperationId(String);

impl OperationId {
    /// The identifier `text` spells, or the reason it spells none.
    pub fn new(text: impl AsRef<str>) -> Result<Self, IllegalOperationId> {
        let text = text.as_ref();
        if text.is_empty() {
            return Err(IllegalOperationId);
        }
        Ok(OperationId(text.to_string()))
    }

    /// The identifier as the string it is.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OperationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A string that spells no operation identifier: the empty one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IllegalOperationId;

impl fmt::Display for IllegalOperationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an operation identifier names something rather than nothing")
    }
}

impl std::error::Error for IllegalOperationId {}

impl<'de> Deserialize<'de> for OperationId {
    /// An identifier arrives as the string it is written as and is read
    /// through the grammar, so the empty string is refused.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        OperationId::new(text).map_err(D::Error::custom)
    }
}

impl JsonSchema for OperationId {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("OperationId")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::OperationId")
    }

    /// A string with the floor of one character the reader keeps.
    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "The identifier an operation is required by: a string naming something rather than nothing.",
            "minLength": 1,
        })
    }
}

/// What one operation changes: its kind, under `kind`, and the fields that
/// kind names, under `fields`.
///
/// On the wire a kind is two keys:
/// `{"kind":"delete_document","fields":{"path":"notes/b.md"}}`. A field the
/// kind does not name is refused.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    content = "fields",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum OperationKind {
    /// Create a document that does not exist yet, holding exactly this
    /// content.
    CreateDocument {
        /// Where the document is created. Nothing may stand there.
        path: DocumentPath,
        /// The document's full text.
        content: String,
    },
    /// Replace one occurrence of a text in a document. The text must occur
    /// exactly once.
    StrReplace {
        /// The document edited.
        path: DocumentPath,
        /// The text replaced, which must occur in the document exactly once.
        old_str: String,
        /// The text it is replaced with.
        new_str: String,
    },
    /// Move a document to a path nothing stands at.
    MoveDocument {
        /// Where the document stands.
        from: DocumentPath,
        /// Where it is moved to. Nothing may stand there, unless another
        /// operation of the same plan moves or removes what does.
        to: DocumentPath,
    },
    /// Remove a document.
    DeleteDocument {
        /// The document removed.
        path: DocumentPath,
    },
}

impl OperationKind {
    /// Create the document at `path`, holding `content`.
    pub fn create_document(path: DocumentPath, content: impl Into<String>) -> Self {
        OperationKind::CreateDocument {
            path,
            content: content.into(),
        }
    }

    /// Replace the one occurrence of `old_str` in the document at `path` with
    /// `new_str`.
    pub fn str_replace(
        path: DocumentPath,
        old_str: impl Into<String>,
        new_str: impl Into<String>,
    ) -> Self {
        OperationKind::StrReplace {
            path,
            old_str: old_str.into(),
            new_str: new_str.into(),
        }
    }

    /// Move the document at `from` to `to`.
    pub const fn move_document(from: DocumentPath, to: DocumentPath) -> Self {
        OperationKind::MoveDocument { from, to }
    }

    /// Remove the document at `path`.
    pub const fn delete_document(path: DocumentPath) -> Self {
        OperationKind::DeleteDocument { path }
    }
}

/// A fact about the vault an operation's author observed and requires to
/// hold.
///
/// On the wire a condition is an object tagged `condition`:
/// `{"condition":"content_hash","path":"notes/a.md","hash":"sha256:…"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "condition", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthorCondition {
    /// The file at the path holds exactly the bytes with this hash. On a file
    /// the plan writes, it becomes that file's before-state; on any other
    /// file, it becomes a condition of the resolved plan.
    ContentHash {
        /// The file observed.
        path: DocumentPath,
        /// The hash of what it held.
        hash: ContentHash,
    },
}

impl AuthorCondition {
    /// The file at `path` holds the bytes whose hash is `hash`.
    pub const fn content_hash(path: DocumentPath, hash: ContentHash) -> Self {
        AuthorCondition::ContentHash { path, hash }
    }
}

/// One change a plan is authored in: a kind and its fields, and optionally an
/// identifier, the operations it requires, a footnote and the conditions its
/// author observed.
///
/// On the wire an operation is one object:
/// `{"kind":"move_document","fields":{"from":"a.md","to":"b.md"},"id":"move-a"}`.
/// The optional parts are left out where they are not written, and a key the
/// operation does not name is refused.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Operation {
    /// What the operation changes.
    #[serde(flatten)]
    pub kind: OperationKind,
    /// The identifier other operations of the same plan require it by.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<OperationId>,
    /// The identifiers of the operations of the same plan this one runs
    /// after.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<OperationId>,
    /// Words about the operation, for a person reading the plan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footnote: Option<String>,
    /// What the author observed and requires to hold.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<AuthorCondition>,
}

impl Operation {
    /// The operation changing what `kind` names, with no optional part.
    pub const fn new(kind: OperationKind) -> Self {
        Operation {
            kind,
            id: None,
            requires: Vec::new(),
            footnote: None,
            conditions: Vec::new(),
        }
    }

    /// The operation identified by `id`.
    #[must_use]
    pub fn with_id(mut self, id: OperationId) -> Self {
        self.id = Some(id);
        self
    }

    /// The operation running after the operations `requires` identifies.
    #[must_use]
    pub fn with_requires(mut self, requires: Vec<OperationId>) -> Self {
        self.requires = requires;
        self
    }

    /// The operation carrying `footnote`.
    #[must_use]
    pub fn with_footnote(mut self, footnote: impl Into<String>) -> Self {
        self.footnote = Some(footnote.into());
        self
    }

    /// The operation requiring `conditions` to hold.
    #[must_use]
    pub fn with_conditions(mut self, conditions: Vec<AuthorCondition>) -> Self {
        self.conditions = conditions;
        self
    }
}

/// The name under an operation's `kind`: one member per [`OperationKind`]
/// variant, read before or after the fields it names.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum KindName {
    CreateDocument,
    StrReplace,
    MoveDocument,
    DeleteDocument,
}

impl KindName {
    /// The name as the wire writes it, for a refusal that names the kind.
    const fn as_str(self) -> &'static str {
        match self {
            KindName::CreateDocument => "create_document",
            KindName::StrReplace => "str_replace",
            KindName::MoveDocument => "move_document",
            KindName::DeleteDocument => "delete_document",
        }
    }
}

/// Every field any kind names, each held as whether it was written. A key no
/// kind names is refused here, a key written twice is refused here, and which
/// of them the kind takes is decided once the kind is known.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KindFields {
    #[serde(default, deserialize_with = "written")]
    path: Option<DocumentPath>,
    #[serde(default, deserialize_with = "written")]
    content: Option<String>,
    #[serde(default, deserialize_with = "written")]
    old_str: Option<String>,
    #[serde(default, deserialize_with = "written")]
    new_str: Option<String>,
    #[serde(default, deserialize_with = "written")]
    from: Option<DocumentPath>,
    #[serde(default, deserialize_with = "written")]
    to: Option<DocumentPath>,
}

/// A field that was written, read as its own type: `null` is a value the
/// type refuses rather than a field left out.
pub(crate) fn written<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// The field `name` of a `kind` operation, which the kind requires.
fn required<T, E: serde::de::Error>(kind: KindName, name: &str, value: Option<T>) -> Result<T, E> {
    value.ok_or_else(|| {
        E::custom(format_args!(
            "a `{}` operation's fields name `{name}`",
            kind.as_str()
        ))
    })
}

/// The field `name` of a `kind` operation, which the kind does not take.
fn refused<E: serde::de::Error>(kind: KindName, name: &str, written: bool) -> Result<(), E> {
    if written {
        return Err(E::custom(format_args!(
            "a `{}` operation's fields do not take `{name}`",
            kind.as_str()
        )));
    }
    Ok(())
}

impl KindFields {
    /// The kind `kind` names, built from exactly the fields it takes: each of
    /// them written, and no field of another kind written.
    fn into_kind<E: serde::de::Error>(self, kind: KindName) -> Result<OperationKind, E> {
        let KindFields {
            path,
            content,
            old_str,
            new_str,
            from,
            to,
        } = self;
        match kind {
            KindName::CreateDocument => {
                refused(kind, "old_str", old_str.is_some())?;
                refused(kind, "new_str", new_str.is_some())?;
                refused(kind, "from", from.is_some())?;
                refused(kind, "to", to.is_some())?;
                Ok(OperationKind::CreateDocument {
                    path: required(kind, "path", path)?,
                    content: required(kind, "content", content)?,
                })
            }
            KindName::StrReplace => {
                refused(kind, "content", content.is_some())?;
                refused(kind, "from", from.is_some())?;
                refused(kind, "to", to.is_some())?;
                Ok(OperationKind::StrReplace {
                    path: required(kind, "path", path)?,
                    old_str: required(kind, "old_str", old_str)?,
                    new_str: required(kind, "new_str", new_str)?,
                })
            }
            KindName::MoveDocument => {
                refused(kind, "path", path.is_some())?;
                refused(kind, "content", content.is_some())?;
                refused(kind, "old_str", old_str.is_some())?;
                refused(kind, "new_str", new_str.is_some())?;
                Ok(OperationKind::MoveDocument {
                    from: required(kind, "from", from)?,
                    to: required(kind, "to", to)?,
                })
            }
            KindName::DeleteDocument => {
                refused(kind, "content", content.is_some())?;
                refused(kind, "old_str", old_str.is_some())?;
                refused(kind, "new_str", new_str.is_some())?;
                refused(kind, "from", from.is_some())?;
                refused(kind, "to", to.is_some())?;
                Ok(OperationKind::DeleteDocument {
                    path: required(kind, "path", path)?,
                })
            }
        }
    }
}

/// The operation as it arrives: every key it may carry and no other, read by
/// the derive in whatever order they are written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationFields {
    kind: KindName,
    fields: KindFields,
    #[serde(default)]
    id: Option<OperationId>,
    #[serde(default)]
    requires: Vec<OperationId>,
    #[serde(default)]
    footnote: Option<String>,
    #[serde(default)]
    conditions: Vec<AuthorCondition>,
}

impl<'de> Deserialize<'de> for Operation {
    /// The keys are read by the derive, refusing any the operation does not
    /// name and any written twice, at its own level and inside its fields;
    /// the fields are then taken as the kind names them, refusing a field the
    /// kind lacks or does not take.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let OperationFields {
            kind,
            fields,
            id,
            requires,
            footnote,
            conditions,
        } = OperationFields::deserialize(deserializer)?;
        Ok(Operation {
            kind: fields.into_kind(kind)?,
            id,
            requires,
            footnote,
            conditions,
        })
    }
}

impl JsonSchema for Operation {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("Operation")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::Operation")
    }

    /// The kind's own schema — one branch per kind, its name a constant under
    /// `kind` and its fields under `fields` — with the operation's optional
    /// parts added to every branch. Each branch already refuses a key it does
    /// not name, so the parts are added inside it rather than beside it.
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let optional_parts = [
            (
                "id",
                generator.subschema_for::<OperationId>(),
                "The identifier other operations of the same plan require it by.",
            ),
            (
                "requires",
                generator.subschema_for::<Vec<OperationId>>(),
                "The identifiers of the operations of the same plan this one runs after.",
            ),
            (
                "footnote",
                generator.subschema_for::<String>(),
                "Words about the operation, for a person reading the plan.",
            ),
            (
                "conditions",
                generator.subschema_for::<Vec<AuthorCondition>>(),
                "What the author observed and requires to hold.",
            ),
        ];
        let mut schema = OperationKind::json_schema(generator);
        transform_subschemas(
            &mut |branch: &mut Schema| {
                let properties: &mut Schema = branch
                    .get_mut("properties")
                    .and_then(|properties| properties.try_into().ok())
                    .expect("a kind's branch describes its properties");
                for (name, part, description) in &optional_parts {
                    let mut part = part.clone();
                    part.insert("description".to_string(), (*description).into());
                    properties.insert((*name).to_string(), part.into());
                }
            },
            &mut schema,
        );
        schema.insert(
            "description".to_string(),
            "One change a plan is authored in: a kind and its fields, and optionally an identifier, the operations it requires, a footnote and the conditions its author observed. The optional parts are left out where they are not written, and a key the operation does not name is refused."
                .into(),
        );
        schema
    }
}
