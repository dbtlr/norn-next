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
//! **An edit's anchor is not a condition.** The text a `str_replace` replaces,
//! and the heading a section kind names, are part of the operation: an
//! operation whose anchor is gone or ambiguous no longer resolves, which is a
//! different outcome from a condition that fails.
//!
//! **The kind map.** Four kinds place, edit, move and remove whole documents:
//! `create_document`, `str_replace`, `move_document` and `delete_document`.
//! Four write one frontmatter field of the documents a target names —
//! `set_frontmatter`, `remove_frontmatter`, `push_frontmatter` and
//! `pop_frontmatter` — and six edit one document's text: `replace_body` and
//! the five section kinds. Push and pop stay kinds of their own rather than
//! compiling to a set of the list they compute, so a plan resolved again
//! after a foreign edit appends to or removes from what the document holds
//! then, rather than writing a list computed before the edit. The legacy
//! `add_frontmatter` is retired, not minted: adding a field only where it is
//! absent is `set_frontmatter` under an expected-value condition of absent,
//! and appending is `push_frontmatter`.
//!
//! **An expected value is the author's condition on one field.** It is
//! checked at planning and becomes the document's before-state, as a content
//! hash on a document the plan writes does. Its observation is tagged
//! `state`, absent or present with a value, as a file state is, so a field
//! holding null is never mistaken for a field that is absent.

use std::borrow::Cow;
use std::fmt;

use schemars::transform::transform_subschemas;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::document::DocumentPath;
use crate::plan::hash::ContentHash;
use crate::plan::value::AuthoredValue;
use crate::plan::write_target::{WriteTarget, settle_flattened_target};
use crate::predicate::Predicate;

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
/// kind does not name is refused. A frontmatter kind names its documents by
/// exactly one of `path` and `where`:
/// `{"kind":"set_frontmatter","fields":{"path":"notes/a.md","field":"status","value":"done"}}`.
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    content = "fields",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[schemars(transform = flattened_targets)]
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
    /// Set a frontmatter field to exactly this value, adding the field where
    /// the document does not carry it.
    SetFrontmatter {
        /// The documents written.
        #[serde(flatten)]
        target: WriteTarget,
        /// The frontmatter key.
        field: String,
        /// The value the field holds after the write, written exactly.
        value: AuthoredValue,
    },
    /// Remove a frontmatter field the document carries.
    RemoveFrontmatter {
        /// The documents written.
        #[serde(flatten)]
        target: WriteTarget,
        /// The frontmatter key removed.
        field: String,
    },
    /// Append a value to a frontmatter list. A field the document does not
    /// carry becomes a list of the one value.
    PushFrontmatter {
        /// The documents written.
        #[serde(flatten)]
        target: WriteTarget,
        /// The frontmatter key.
        field: String,
        /// The value appended.
        value: AuthoredValue,
    },
    /// Remove every element equal to a value from a frontmatter list. The
    /// list must hold the value.
    PopFrontmatter {
        /// The documents written.
        #[serde(flatten)]
        target: WriteTarget,
        /// The frontmatter key.
        field: String,
        /// The value removed, wherever the list holds it.
        value: AuthoredValue,
    },
    /// Replace everything after the frontmatter block, leaving the block
    /// exactly as it is.
    ReplaceBody {
        /// The document edited.
        path: DocumentPath,
        /// The body's full text.
        content: String,
    },
    /// Replace the body of a section, keeping its heading line.
    ReplaceSection {
        /// The document edited.
        path: DocumentPath,
        /// The heading's exact text, without its `#` marks. It must head
        /// exactly one section of the document, at any level.
        heading: String,
        /// The section's new body.
        content: String,
    },
    /// Append text to the end of a section's body.
    AppendToSection {
        /// The document edited.
        path: DocumentPath,
        /// The heading's exact text, without its `#` marks. It must head
        /// exactly one section of the document, at any level.
        heading: String,
        /// The text appended.
        content: String,
    },
    /// Remove a section: its heading line and its body.
    DeleteSection {
        /// The document edited.
        path: DocumentPath,
        /// The heading's exact text, without its `#` marks. It must head
        /// exactly one section of the document, at any level.
        heading: String,
    },
    /// Insert text before a heading line.
    InsertBeforeHeading {
        /// The document edited.
        path: DocumentPath,
        /// The heading's exact text, without its `#` marks. It must head
        /// exactly one section of the document, at any level.
        heading: String,
        /// The text inserted.
        content: String,
    },
    /// Insert text after a heading line.
    InsertAfterHeading {
        /// The document edited.
        path: DocumentPath,
        /// The heading's exact text, without its `#` marks. It must head
        /// exactly one section of the document, at any level.
        heading: String,
        /// The text inserted.
        content: String,
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

    /// Set `field` to `value` in the documents `target` names.
    pub fn set_frontmatter(
        target: WriteTarget,
        field: impl Into<String>,
        value: AuthoredValue,
    ) -> Self {
        OperationKind::SetFrontmatter {
            target,
            field: field.into(),
            value,
        }
    }

    /// Remove `field` from the documents `target` names.
    pub fn remove_frontmatter(target: WriteTarget, field: impl Into<String>) -> Self {
        OperationKind::RemoveFrontmatter {
            target,
            field: field.into(),
        }
    }

    /// Append `value` to the list `field` holds in the documents `target`
    /// names.
    pub fn push_frontmatter(
        target: WriteTarget,
        field: impl Into<String>,
        value: AuthoredValue,
    ) -> Self {
        OperationKind::PushFrontmatter {
            target,
            field: field.into(),
            value,
        }
    }

    /// Remove every element equal to `value` from the list `field` holds in
    /// the documents `target` names.
    pub fn pop_frontmatter(
        target: WriteTarget,
        field: impl Into<String>,
        value: AuthoredValue,
    ) -> Self {
        OperationKind::PopFrontmatter {
            target,
            field: field.into(),
            value,
        }
    }

    /// Replace the body of the document at `path` with `content`.
    pub fn replace_body(path: DocumentPath, content: impl Into<String>) -> Self {
        OperationKind::ReplaceBody {
            path,
            content: content.into(),
        }
    }

    /// Replace the body of the section `heading` heads in the document at
    /// `path` with `content`.
    pub fn replace_section(
        path: DocumentPath,
        heading: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        OperationKind::ReplaceSection {
            path,
            heading: heading.into(),
            content: content.into(),
        }
    }

    /// Append `content` to the section `heading` heads in the document at
    /// `path`.
    pub fn append_to_section(
        path: DocumentPath,
        heading: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        OperationKind::AppendToSection {
            path,
            heading: heading.into(),
            content: content.into(),
        }
    }

    /// Remove the section `heading` heads in the document at `path`.
    pub fn delete_section(path: DocumentPath, heading: impl Into<String>) -> Self {
        OperationKind::DeleteSection {
            path,
            heading: heading.into(),
        }
    }

    /// Insert `content` before the heading line `heading` in the document at
    /// `path`.
    pub fn insert_before_heading(
        path: DocumentPath,
        heading: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        OperationKind::InsertBeforeHeading {
            path,
            heading: heading.into(),
            content: content.into(),
        }
    }

    /// Insert `content` after the heading line `heading` in the document at
    /// `path`.
    pub fn insert_after_heading(
        path: DocumentPath,
        heading: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        OperationKind::InsertAfterHeading {
            path,
            heading: heading.into(),
            content: content.into(),
        }
    }

    /// The kind's name, as the wire writes it under `kind`.
    pub const fn name(&self) -> &'static str {
        self.kind_name().as_str()
    }

    /// The member of the name vocabulary this kind is read under.
    const fn kind_name(&self) -> KindName {
        match self {
            OperationKind::CreateDocument { .. } => KindName::CreateDocument,
            OperationKind::StrReplace { .. } => KindName::StrReplace,
            OperationKind::MoveDocument { .. } => KindName::MoveDocument,
            OperationKind::DeleteDocument { .. } => KindName::DeleteDocument,
            OperationKind::SetFrontmatter { .. } => KindName::SetFrontmatter,
            OperationKind::RemoveFrontmatter { .. } => KindName::RemoveFrontmatter,
            OperationKind::PushFrontmatter { .. } => KindName::PushFrontmatter,
            OperationKind::PopFrontmatter { .. } => KindName::PopFrontmatter,
            OperationKind::ReplaceBody { .. } => KindName::ReplaceBody,
            OperationKind::ReplaceSection { .. } => KindName::ReplaceSection,
            OperationKind::AppendToSection { .. } => KindName::AppendToSection,
            OperationKind::DeleteSection { .. } => KindName::DeleteSection,
            OperationKind::InsertBeforeHeading { .. } => KindName::InsertBeforeHeading,
            OperationKind::InsertAfterHeading { .. } => KindName::InsertAfterHeading,
        }
    }

    /// The target of a frontmatter kind, and `None` for every other kind,
    /// which names its document by path alone.
    pub const fn target(&self) -> Option<&WriteTarget> {
        match self {
            OperationKind::SetFrontmatter { target, .. }
            | OperationKind::RemoveFrontmatter { target, .. }
            | OperationKind::PushFrontmatter { target, .. }
            | OperationKind::PopFrontmatter { target, .. } => Some(target),
            OperationKind::CreateDocument { .. }
            | OperationKind::StrReplace { .. }
            | OperationKind::MoveDocument { .. }
            | OperationKind::DeleteDocument { .. }
            | OperationKind::ReplaceBody { .. }
            | OperationKind::ReplaceSection { .. }
            | OperationKind::AppendToSection { .. }
            | OperationKind::DeleteSection { .. }
            | OperationKind::InsertBeforeHeading { .. }
            | OperationKind::InsertAfterHeading { .. } => None,
        }
    }
}

/// The derived schema of every kind, with a frontmatter kind's fields — its
/// target flattened among them — written the way every other kind's fields
/// are.
fn flattened_targets(schema: &mut Schema) {
    transform_subschemas(
        &mut |branch: &mut Schema| {
            let fields: Option<&mut Schema> = branch
                .get_mut("properties")
                .and_then(|properties| properties.get_mut("fields"))
                .and_then(|fields| fields.try_into().ok());
            if let Some(fields) = fields {
                settle_flattened_target(fields);
            }
        },
        schema,
    );
}

/// A kind as it arrives on its own: its name and its fields, and no other
/// key.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KindKeys {
    kind: KindName,
    fields: KindFields,
}

impl<'de> Deserialize<'de> for OperationKind {
    /// A kind read on its own is read as an operation's kind is: the derive
    /// reads its two keys and every field any kind names, refusing a key
    /// written twice, and the kind then takes the fields it names.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let KindKeys { kind, fields } = KindKeys::deserialize(deserializer)?;
        fields.into_kind(kind)
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
    /// The document at the path carries a frontmatter field holding exactly
    /// this value, or does not carry the field. Checked at planning, it
    /// becomes that document's before-state.
    ExpectedValue {
        /// The document observed.
        path: DocumentPath,
        /// The frontmatter key.
        field: String,
        /// What the field held: absent, or present with its value.
        expect: ExpectedField,
    },
}

impl AuthorCondition {
    /// The file at `path` holds the bytes whose hash is `hash`.
    pub const fn content_hash(path: DocumentPath, hash: ContentHash) -> Self {
        AuthorCondition::ContentHash { path, hash }
    }

    /// The document at `path` holds `expect` under `field`.
    pub fn expected_value(
        path: DocumentPath,
        field: impl Into<String>,
        expect: ExpectedField,
    ) -> Self {
        AuthorCondition::ExpectedValue {
            path,
            field: field.into(),
            expect,
        }
    }
}

/// What an author observed a frontmatter field holding: nothing, or exactly
/// a value.
///
/// On the wire an observation is an object tagged `state`, as a file state
/// is: `{"state":"absent"}`, `{"state":"present","value":"draft"}`. A field
/// holding null is present with the value `null`, never absent.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExpectedField {
    /// The document does not carry the field.
    Absent {},
    /// The document carries the field, holding exactly this value.
    Present {
        /// The value the field holds.
        value: AuthoredValue,
    },
}

impl ExpectedField {
    /// The document does not carry the field.
    pub const fn absent() -> Self {
        ExpectedField::Absent {}
    }

    /// The field holds exactly `value`.
    pub const fn present(value: AuthoredValue) -> Self {
        ExpectedField::Present { value }
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
    SetFrontmatter,
    RemoveFrontmatter,
    PushFrontmatter,
    PopFrontmatter,
    ReplaceBody,
    ReplaceSection,
    AppendToSection,
    DeleteSection,
    InsertBeforeHeading,
    InsertAfterHeading,
}

impl KindName {
    /// The name as the wire writes it, for a refusal that names the kind.
    const fn as_str(self) -> &'static str {
        match self {
            KindName::CreateDocument => "create_document",
            KindName::StrReplace => "str_replace",
            KindName::MoveDocument => "move_document",
            KindName::DeleteDocument => "delete_document",
            KindName::SetFrontmatter => "set_frontmatter",
            KindName::RemoveFrontmatter => "remove_frontmatter",
            KindName::PushFrontmatter => "push_frontmatter",
            KindName::PopFrontmatter => "pop_frontmatter",
            KindName::ReplaceBody => "replace_body",
            KindName::ReplaceSection => "replace_section",
            KindName::AppendToSection => "append_to_section",
            KindName::DeleteSection => "delete_section",
            KindName::InsertBeforeHeading => "insert_before_heading",
            KindName::InsertAfterHeading => "insert_after_heading",
        }
    }

    /// The fields the kind takes, as the wire writes them. A frontmatter
    /// kind's target is one of `path` and `where`, so it takes both keys and
    /// the target decides which one is written.
    const fn takes(self) -> &'static [&'static str] {
        match self {
            KindName::CreateDocument | KindName::ReplaceBody => &["path", "content"],
            KindName::StrReplace => &["path", "old_str", "new_str"],
            KindName::MoveDocument => &["from", "to"],
            KindName::DeleteDocument => &["path"],
            KindName::SetFrontmatter | KindName::PushFrontmatter | KindName::PopFrontmatter => {
                &["path", "where", "field", "value"]
            }
            KindName::RemoveFrontmatter => &["path", "where", "field"],
            KindName::ReplaceSection
            | KindName::AppendToSection
            | KindName::InsertBeforeHeading
            | KindName::InsertAfterHeading => &["path", "heading", "content"],
            KindName::DeleteSection => &["path", "heading"],
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
    #[serde(default, rename = "where", deserialize_with = "written")]
    predicates: Option<Vec<Predicate>>,
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
    #[serde(default, deserialize_with = "written")]
    field: Option<String>,
    #[serde(default, deserialize_with = "written")]
    value: Option<AuthoredValue>,
    #[serde(default, deserialize_with = "written")]
    heading: Option<String>,
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

/// The target of a `kind` operation, from its two keys: exactly one of them
/// written.
fn target<E: serde::de::Error>(
    kind: KindName,
    path: Option<DocumentPath>,
    predicates: Option<Vec<Predicate>>,
) -> Result<WriteTarget, E> {
    WriteTarget::from_keys(path, predicates).map_err(|problem| {
        E::custom(format_args!(
            "a `{}` operation's fields name its documents: {problem}",
            kind.as_str()
        ))
    })
}

impl KindFields {
    /// Each field any kind names, and whether it was written.
    const fn written_names(&self) -> [(&'static str, bool); 10] {
        [
            ("path", self.path.is_some()),
            ("where", self.predicates.is_some()),
            ("content", self.content.is_some()),
            ("old_str", self.old_str.is_some()),
            ("new_str", self.new_str.is_some()),
            ("from", self.from.is_some()),
            ("to", self.to.is_some()),
            ("field", self.field.is_some()),
            ("value", self.value.is_some()),
            ("heading", self.heading.is_some()),
        ]
    }

    /// The kind `kind` names, built from exactly the fields it takes: each of
    /// them written, and no field of another kind written.
    fn into_kind<E: serde::de::Error>(self, kind: KindName) -> Result<OperationKind, E> {
        for (name, written) in self.written_names() {
            if written && !kind.takes().contains(&name) {
                return Err(E::custom(format_args!(
                    "a `{}` operation's fields do not take `{name}`",
                    kind.as_str()
                )));
            }
        }
        let KindFields {
            path,
            predicates,
            content,
            old_str,
            new_str,
            from,
            to,
            field,
            value,
            heading,
        } = self;
        Ok(match kind {
            KindName::CreateDocument => OperationKind::CreateDocument {
                path: required(kind, "path", path)?,
                content: required(kind, "content", content)?,
            },
            KindName::StrReplace => OperationKind::StrReplace {
                path: required(kind, "path", path)?,
                old_str: required(kind, "old_str", old_str)?,
                new_str: required(kind, "new_str", new_str)?,
            },
            KindName::MoveDocument => OperationKind::MoveDocument {
                from: required(kind, "from", from)?,
                to: required(kind, "to", to)?,
            },
            KindName::DeleteDocument => OperationKind::DeleteDocument {
                path: required(kind, "path", path)?,
            },
            KindName::SetFrontmatter => OperationKind::SetFrontmatter {
                target: target(kind, path, predicates)?,
                field: required(kind, "field", field)?,
                value: required(kind, "value", value)?,
            },
            KindName::RemoveFrontmatter => OperationKind::RemoveFrontmatter {
                target: target(kind, path, predicates)?,
                field: required(kind, "field", field)?,
            },
            KindName::PushFrontmatter => OperationKind::PushFrontmatter {
                target: target(kind, path, predicates)?,
                field: required(kind, "field", field)?,
                value: required(kind, "value", value)?,
            },
            KindName::PopFrontmatter => OperationKind::PopFrontmatter {
                target: target(kind, path, predicates)?,
                field: required(kind, "field", field)?,
                value: required(kind, "value", value)?,
            },
            KindName::ReplaceBody => OperationKind::ReplaceBody {
                path: required(kind, "path", path)?,
                content: required(kind, "content", content)?,
            },
            KindName::ReplaceSection => OperationKind::ReplaceSection {
                path: required(kind, "path", path)?,
                heading: required(kind, "heading", heading)?,
                content: required(kind, "content", content)?,
            },
            KindName::AppendToSection => OperationKind::AppendToSection {
                path: required(kind, "path", path)?,
                heading: required(kind, "heading", heading)?,
                content: required(kind, "content", content)?,
            },
            KindName::DeleteSection => OperationKind::DeleteSection {
                path: required(kind, "path", path)?,
                heading: required(kind, "heading", heading)?,
            },
            KindName::InsertBeforeHeading => OperationKind::InsertBeforeHeading {
                path: required(kind, "path", path)?,
                heading: required(kind, "heading", heading)?,
                content: required(kind, "content", content)?,
            },
            KindName::InsertAfterHeading => OperationKind::InsertAfterHeading {
                path: required(kind, "path", path)?,
                heading: required(kind, "heading", heading)?,
                content: required(kind, "content", content)?,
            },
        })
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
    /// not name, so the parts are added inside it rather than beside it. Each
    /// part is advertised as the reader takes it: the identifier and the
    /// footnote admit `null`, read as absent, as a derived optional field
    /// advertises; the two lists do not.
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let optional_parts = [
            (
                "id",
                generator.subschema_for::<Option<OperationId>>(),
                "The identifier other operations of the same plan require it by.",
            ),
            (
                "requires",
                generator.subschema_for::<Vec<OperationId>>(),
                "The identifiers of the operations of the same plan this one runs after.",
            ),
            (
                "footnote",
                generator.subschema_for::<Option<String>>(),
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
