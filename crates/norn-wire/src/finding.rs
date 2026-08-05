//! What a finding is filed under, as one closed list.
//!
//! A finding kind is the cause class a reader dispatches on, and it is a code
//! in the same grammar refusals use — so the kind a store row carries, the
//! kind a report renders and the kind a client filters by are one string with
//! one spelling. Derivation records kinds; it does not name them, and a kind
//! nobody can enumerate here is a kind no surface can advertise.
//!
//! The set is `document/…` because each member is a fact about one document:
//! a vault holding a document norn cannot decode stays serviceable, and the
//! finding is where that document's absence from derived state is stated.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The cause class a finding is filed under.
///
/// A kind is a flat namespaced string — `namespace/what-happened` — and the
/// list holds every kind the system can record today.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub enum FindingKind {
    /// `document/path-bytes-not-utf8` — the path's bytes are not UTF-8, so the
    /// document has no name derived state can hold.
    #[serde(rename = "document/path-bytes-not-utf8")]
    PathBytesNotUtf8,
    /// `document/path-names-no-document` — the path is UTF-8 and spells no
    /// document path.
    #[serde(rename = "document/path-names-no-document")]
    PathNamesNoDocument,
    /// `document/body-bytes-not-utf8` — the document's bytes are not UTF-8, so
    /// no facts are read out of it.
    #[serde(rename = "document/body-bytes-not-utf8")]
    BodyBytesNotUtf8,
}

impl FindingKind {
    /// The kind as the string it is on the wire.
    pub const fn as_str(&self) -> &'static str {
        match self {
            FindingKind::PathBytesNotUtf8 => "document/path-bytes-not-utf8",
            FindingKind::PathNamesNoDocument => "document/path-names-no-document",
            FindingKind::BodyBytesNotUtf8 => "document/body-bytes-not-utf8",
        }
    }
}

impl fmt::Display for FindingKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}
