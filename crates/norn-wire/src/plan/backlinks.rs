//! What a delete does about the links naming its document.
//!
//! **One choice, spelled as two keys.** A delete forbids any link to name its
//! document, rewrites every one to name another, or leaves every one broken.
//! In memory that is one of three values, so a delete that both rewrites and
//! breaks its links cannot be built. On the wire it is `rewrite_to` or
//! `allow_broken_links: true` among the fields that name the delete, each
//! written only where it is said, so a delete saying neither is written as
//! its path alone. An `allow_broken_links` of `false` reads as left out;
//! `rewrite_to` beside an `allow_broken_links` of `true` is refused at the
//! read, and the schema states the same exclusion as a `not`.
//!
//! **What each plans.** A link naming the document is one resolving to it
//! alone where the plan leaves the vault, so a link naming several documents
//! names none of them and a link the document holds goes with it. A delete
//! forbidding the links does not resolve while one names its document, and
//! is left unresolved naming every holder and how many links. One rewriting
//! them carries the cascade respelling each, in its own form, to name
//! `rewrite_to`, which must name one document where the plan leaves the
//! vault. One leaving them broken lands, its forecast advising on each.
//!
//! **`rewrite_to` names a whole document.** Each rewritten link keeps the
//! anchor it was written with, so an anchor on `rewrite_to` is refused
//! ([`ResolutionTarget::whole_document`]).

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

use crate::target::{ResolutionTarget, whole_document_schema};

/// What a delete does about the links naming its document: forbids them,
/// rewrites them to name another document, or leaves them broken.
///
/// On the wire it is at most one key among the fields that name the delete:
/// none, `"rewrite_to":"notes/c"`, or `"allow_broken_links":true`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Backlinks {
    /// No link may name the document: a delete any link names does not
    /// resolve.
    Forbidden,
    /// Every link naming the document is rewritten to name this one.
    RewrittenTo(ResolutionTarget),
    /// Every link naming the document is left broken.
    LeftBroken,
}

impl Backlinks {
    /// What the two keys say, as the operation's and the request's readers
    /// take them: `rewrite_to`, already read as a whole document, or
    /// `allow_broken_links`, `false` where it was left out — never both.
    pub(crate) fn from_keys(
        rewrite_to: Option<ResolutionTarget>,
        allow_broken_links: bool,
    ) -> Result<Self, &'static str> {
        match (rewrite_to, allow_broken_links) {
            (None, false) => Ok(Backlinks::Forbidden),
            (Some(target), false) => Ok(Backlinks::RewrittenTo(target)),
            (None, true) => Ok(Backlinks::LeftBroken),
            (Some(_), true) => Err(
                "the links naming the deleted document are rewritten to `rewrite_to` or left broken, not both",
            ),
        }
    }
}

impl Serialize for Backlinks {
    /// Written as the one key that says it, or as nothing, so flattened among
    /// a delete's fields it is that key beside them.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut keys = serializer.serialize_struct("Backlinks", 1)?;
        match self {
            Backlinks::Forbidden => {}
            Backlinks::RewrittenTo(target) => keys.serialize_field("rewrite_to", target)?,
            Backlinks::LeftBroken => keys.serialize_field("allow_broken_links", &true)?,
        }
        keys.end()
    }
}

impl JsonSchema for Backlinks {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("Backlinks")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::Backlinks")
    }

    /// The two keys, both optional, and the one pair the reader refuses
    /// stated as a `not`. It carries no description of its own: it is only
    /// ever flattened among the fields of an object that has one.
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let mut rewrite_to = whole_document_schema(generator);
        rewrite_to.insert(
            "description".to_string(),
            "The document every link naming the removed one is rewritten to name: a path suffix with no anchor, since each link keeps its own. Never written beside an `allow_broken_links` of `true`.".into(),
        );
        json_schema!({
            "type": "object",
            "properties": {
                "rewrite_to": rewrite_to,
                "allow_broken_links": {
                    "type": "boolean",
                    "description": "Whether the links naming the removed document are left broken. Absent is `false`, and `false` is left out.",
                },
            },
            "not": {
                "required": ["rewrite_to", "allow_broken_links"],
                "properties": {"allow_broken_links": {"const": true}},
            },
            "additionalProperties": false,
        })
    }
}
