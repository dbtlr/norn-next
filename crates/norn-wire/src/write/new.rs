//! `new`: create one document, at a path or by a creation rule.
//!
//! **A `new` is exactly one of three forms.** A path with the document's full
//! text creates exactly that content there. A rule name, sent as `as`, with
//! optional `variables`, `fields` and `body`, creates the document the
//! schema's creation rule of that name makes. Neither a path nor a rule
//! names the vault's inbox: a capture with optional `fields` and `body`. A
//! request mixing the forms — a path beside a rule, content without its path,
//! variables without a rule, fields or a body beside a path — is refused at
//! the read, naming the rule, rather than read as one of them.
//!
//! **The first form compiles to a `create_document` and the others to one
//! `create_by_rule`.** Planning expands a `create_by_rule` into the
//! `create_document` its rule makes, a concrete path and the composed text.
//! Whichever way it is created, nothing may stand at the path; the folders
//! above it are always made.
//!
//! **The request is read by hand, because its keys are read together.** The
//! derive reads every key it may carry as written into a private shape,
//! refusing any other and any written twice; the keys are then read as one
//! [`NewSubject`]. That shape is also the schema the request advertises.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::address::VaultAddress;
use crate::apply::ApplyMode;
use crate::document::DocumentPath;
use crate::plan::document::AuthoredPlan;
use crate::plan::document::is_false;
use crate::plan::operation::{
    AuthorCondition, Operation, OperationKind, settle_written_properties, written,
};
use crate::plan::value::{ValueMap, Variables};

/// What a `new` creates: the document at a path, the document a creation
/// rule makes, or an inbox capture.
///
/// On the wire it is the request's own keys: `"path":"inbox/a.md","content":"# A\n"`,
/// `"as":"meeting","variables":{"project":"norn"},"fields":{"status":"draft"},"body":"…"`,
/// or, for the inbox, `"fields":{…},"body":"…"` with neither `path` nor `as`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum NewSubject {
    /// The document at `path`, holding exactly `content`.
    #[non_exhaustive]
    Document {
        /// Where the document is created. Nothing may stand there.
        path: DocumentPath,
        /// The document's full text.
        content: String,
    },
    /// The document the creation rule `rule` makes.
    #[non_exhaustive]
    Rule {
        /// The creation rule's name, sent as `as`.
        #[serde(rename = "as")]
        rule: String,
        /// The values the rule's path and template take, by variable name.
        #[serde(skip_serializing_if = "Variables::is_empty")]
        variables: Variables,
        /// The frontmatter fields the document is created with.
        #[serde(skip_serializing_if = "ValueMap::is_empty")]
        fields: ValueMap,
        /// The document's body.
        #[serde(skip_serializing_if = "Option::is_none")]
        body: Option<String>,
    },
    /// A capture into the vault's inbox, naming no rule and no path.
    #[non_exhaustive]
    Inbox {
        /// The frontmatter fields the document is created with.
        #[serde(skip_serializing_if = "ValueMap::is_empty")]
        fields: ValueMap,
        /// The document's body.
        #[serde(skip_serializing_if = "Option::is_none")]
        body: Option<String>,
    },
}

impl NewSubject {
    /// The document at `path`, holding `content`.
    pub fn document(path: DocumentPath, content: impl Into<String>) -> Self {
        NewSubject::Document {
            path,
            content: content.into(),
        }
    }

    /// The document the creation rule `rule` makes from `variables`, `fields`
    /// and `body`.
    pub fn by_rule(
        rule: impl Into<String>,
        variables: Variables,
        fields: ValueMap,
        body: Option<String>,
    ) -> Self {
        NewSubject::Rule {
            rule: rule.into(),
            variables,
            fields,
            body,
        }
    }

    /// A capture into the vault's inbox of `fields` and `body`.
    pub const fn inbox(fields: ValueMap, body: Option<String>) -> Self {
        NewSubject::Inbox { fields, body }
    }

    /// The operation kind this creation is.
    fn into_kind(self) -> OperationKind {
        match self {
            NewSubject::Document { path, content } => {
                OperationKind::CreateDocument { path, content }
            }
            NewSubject::Rule {
                rule,
                variables,
                fields,
                body,
            } => OperationKind::create_by_rule(Some(rule), variables, fields, body),
            NewSubject::Inbox { fields, body } => {
                OperationKind::create_by_rule(None, Variables::default(), fields, body)
            }
        }
    }

    /// The one form the written keys make, or what mixes them.
    fn from_keys(keys: NewForm) -> Result<Self, String> {
        let NewForm {
            path,
            content,
            rule,
            variables,
            fields,
            body,
        } = keys;
        if rule.as_deref() == Some("") {
            return Err(
                "`as` is empty, and an empty name names no rule: leave it out for the inbox"
                    .to_string(),
            );
        }
        match (path, content, rule) {
            (Some(path), Some(content), None) => {
                let extras: Vec<&str> = [
                    ("variables", variables.is_some()),
                    ("fields", fields.is_some()),
                    ("body", body.is_some()),
                ]
                .into_iter()
                .filter_map(|(name, written)| written.then_some(name))
                .collect();
                if extras.is_empty() {
                    Ok(NewSubject::Document { path, content })
                } else {
                    Err(format!(
                        "`path` with `content` takes none of `variables`, `fields` and `body`, and `{}` is written",
                        extras.join("`, `")
                    ))
                }
            }
            (Some(_), None, None) => Err("`path` is written without `content`".to_string()),
            (None, Some(_), _) => Err("`content` is written without `path`".to_string()),
            (Some(_), _, Some(_)) => Err("`path` and `as` are both written".to_string()),
            (None, None, Some(rule)) => Ok(NewSubject::Rule {
                rule,
                variables: variables.unwrap_or_default(),
                fields: fields.unwrap_or_default(),
                body,
            }),
            (None, None, None) => {
                if variables.is_some() {
                    return Err("`variables` is written without `as`".to_string());
                }
                Ok(NewSubject::Inbox {
                    fields: fields.unwrap_or_default(),
                    body,
                })
            }
        }
    }
}

/// What a `new` request carries.
///
/// On the wire the subject's keys sit beside the request's other keys:
/// `{"vault":…,"mode":"preview","path":"inbox/a.md","content":"# A\n"}`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub struct NewParams {
    /// The vault written.
    pub vault: VaultAddress,
    /// Whether to preview the write or apply it. There is no default.
    pub mode: ApplyMode,
    /// What is created.
    #[serde(flatten)]
    pub subject: NewSubject,
    /// What the author observed and requires to hold.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<AuthorCondition>,
    /// Whether the write applies past the schema check. Absent is `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub force: bool,
}

impl NewParams {
    /// A request to `mode` the creation of the document at `path` in `vault`,
    /// holding `content`, with no condition and not forced.
    pub fn new(
        vault: VaultAddress,
        mode: ApplyMode,
        path: DocumentPath,
        content: impl Into<String>,
    ) -> Self {
        NewParams::for_subject(vault, mode, NewSubject::document(path, content))
    }

    /// A request to `mode` the creation `subject` names in `vault`, with no
    /// condition and not forced.
    pub const fn for_subject(vault: VaultAddress, mode: ApplyMode, subject: NewSubject) -> Self {
        NewParams {
            vault,
            mode,
            subject,
            conditions: Vec::new(),
            force: false,
        }
    }

    /// The request requiring `conditions` to hold.
    #[must_use]
    pub fn with_conditions(mut self, conditions: Vec<AuthorCondition>) -> Self {
        self.conditions = conditions;
        self
    }

    /// The request applying past the schema check where `force` holds.
    #[must_use]
    pub const fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }

    /// The plan the request compiles to: one `create_document` for a path
    /// and its content, and one `create_by_rule` for a rule or the inbox,
    /// carrying the request's conditions, forced as the request is.
    pub fn plan(self) -> AuthoredPlan {
        let NewParams {
            vault,
            mode: _,
            subject,
            conditions,
            force,
        } = self;
        let operation = Operation::new(subject.into_kind()).with_conditions(conditions);
        AuthoredPlan::new(vault, vec![operation]).with_force(force)
    }
}

/// The keys that make a [`NewSubject`], each as written.
struct NewForm {
    path: Option<DocumentPath>,
    content: Option<String>,
    rule: Option<String>,
    variables: Option<Variables>,
    fields: Option<ValueMap>,
    body: Option<String>,
}

/// What a `new` request carries: one of three forms, by which of its keys are
/// written.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "NewParams", transform = settle_written_properties)]
struct NewKeys {
    /// The vault written.
    vault: VaultAddress,
    /// Whether to preview the write or apply it. There is no default.
    mode: ApplyMode,
    /// Where the document is created, with `content`. Nothing may stand
    /// there. Written with neither `as` nor `variables`, `fields` or `body`.
    #[serde(default, deserialize_with = "written")]
    path: Option<DocumentPath>,
    /// The document's full text, with `path`.
    #[serde(default, deserialize_with = "written")]
    content: Option<String>,
    /// The creation rule that makes the document, by name. Written with
    /// neither `path` nor `content`. A request naming neither `path` nor `as`
    /// is a capture into the vault's inbox.
    #[serde(default, rename = "as", deserialize_with = "written")]
    #[schemars(rename = "as", length(min = 1))]
    rule: Option<String>,
    /// The values the rule's path and template take, by variable name. Only
    /// with `as`.
    #[serde(default, deserialize_with = "written")]
    variables: Option<Variables>,
    /// The frontmatter fields the document is created with. Not with `path`.
    #[serde(default, deserialize_with = "written")]
    fields: Option<ValueMap>,
    /// The document's body. Not with `path`.
    #[serde(default, deserialize_with = "written")]
    body: Option<String>,
    /// What the author observed and requires to hold.
    #[serde(default)]
    conditions: Vec<AuthorCondition>,
    /// Whether the write applies past the schema check. Absent is `false`.
    #[serde(default)]
    force: bool,
}

impl<'de> Deserialize<'de> for NewParams {
    /// Every key is read by the derive, refusing any the request does not
    /// name and any written twice; the keys are then read as one of the
    /// three forms, refusing a mix of them.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let NewKeys {
            vault,
            mode,
            path,
            content,
            rule,
            variables,
            fields,
            body,
            conditions,
            force,
        } = NewKeys::deserialize(deserializer)?;
        let subject = NewSubject::from_keys(NewForm {
            path,
            content,
            rule,
            variables,
            fields,
            body,
        })
        .map_err(|problem| {
            D::Error::custom(format_args!(
                "a `new` takes exactly one of three forms — `path` with `content`; `as` with optional `variables`, `fields` and `body`; or neither `path` nor `as`, with optional `fields` and `body`: {problem}"
            ))
        })?;
        Ok(NewParams {
            vault,
            mode,
            subject,
            conditions,
            force,
        })
    }
}

impl JsonSchema for NewParams {
    fn schema_name() -> Cow<'static, str> {
        NewKeys::schema_name()
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::NewParams")
    }

    /// The shape the request is read through, which carries every key of the
    /// three forms as written.
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        NewKeys::json_schema(generator)
    }
}
