//! What a finding is filed under and how urgently it is reported, as two
//! closed lists.
//!
//! A finding kind is the cause class a reader dispatches on, and it is a code
//! in the same grammar refusals use — so the kind a store row carries, the
//! kind a report renders and the kind a client filters by are one string with
//! one spelling. Derivation records kinds; it does not name them, and a kind
//! nobody can enumerate here is a kind no surface can advertise.
//!
//! The set is `document/…` because each member is a fact about one document:
//! a vault holding a document norn cannot fully read stays serviceable, and the
//! finding is where what is missing from derived state is stated.
//!
//! **A kind also says where its findings may stand**, as [`FindingScope`]. A
//! kind whose cause leaves nothing derivable is about the *place* and stands
//! only where no document row does; a kind whose cause leaves the document
//! derivable but incomplete is about the *document* and stands beside its row.
//! A client reading the findings at a path branches on that: a place-scoped
//! finding there says no document is derived at it, and a document-scoped one
//! says the document beside it is derived with something missing.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How urgently a finding is reported.
///
/// Severity is presentation and filtering vocabulary, not the cause class a
/// reader dispatches on. The list holds every severity the system can record
/// today.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// The finding describes state that must be corrected.
    Error,
    /// The finding describes state that deserves attention.
    Warning,
}

impl Severity {
    /// Every severity the registry holds, in declaration order.
    pub const ALL: [Severity; 2] = [Severity::Error, Severity::Warning];

    /// The severity as the string stored and carried on the wire.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A string that spells no severity the registry holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnknownSeverity;

impl fmt::Display for UnknownSeverity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the string spells no finding severity")
    }
}

impl std::error::Error for UnknownSeverity {}

impl TryFrom<&str> for Severity {
    type Error = UnknownSeverity;

    fn try_from(string: &str) -> Result<Self, UnknownSeverity> {
        Self::ALL
            .into_iter()
            .find(|severity| severity.as_str() == string)
            .ok_or(UnknownSeverity)
    }
}

/// The cause class a finding is filed under.
///
/// A kind is a flat namespaced string — `namespace/what-happened` — and the
/// list holds every kind the system can record today.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
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
    /// `document/frontmatter-too-large` — the frontmatter block is past the
    /// bound the text layer reads, so the block is refused unparsed and the
    /// document's fields are unknown.
    #[serde(rename = "document/frontmatter-too-large")]
    FrontmatterTooLarge,
    /// `document/frontmatter-unclosed` — the frontmatter block opens and never
    /// closes, so no block's bytes are addressable and the document's fields
    /// are unknown.
    #[serde(rename = "document/frontmatter-unclosed")]
    FrontmatterUnclosed,
    /// `document/frontmatter-unreadable` — the frontmatter block is not
    /// well-formed, so nothing parsed it and the document's fields are unknown.
    #[serde(rename = "document/frontmatter-unreadable")]
    FrontmatterUnreadable,
}

/// Where the findings of a kind may stand.
///
/// The two scopes differ in one thing: whether a document row at the subject
/// withholds the finding. Everything else — how a finding is recorded, read,
/// filtered and discarded — is the same for both.
///
/// Plain rather than `#[non_exhaustive]`: a producer or a reader that has not
/// decided what a new scope means should fail to compile rather than fall into
/// a default arm.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingScope {
    /// The finding is about the place, and no document is derived there. It is
    /// withheld while a document row stands at its subject, because a place a
    /// readable document occupies is that document's.
    Place,
    /// The finding is about the document derived at its subject, and stands
    /// beside that document's row. Withholding it would suppress every finding
    /// it exists to report.
    Document,
}

impl FindingKind {
    /// Every kind the registry holds, in declaration order.
    ///
    /// Reading a kind back and enumerating the registry both walk this list,
    /// so a variant absent here is unreadable and unadvertisable — the schema
    /// suite holds this list equal to the enum itself.
    pub const ALL: [FindingKind; 6] = [
        FindingKind::PathBytesNotUtf8,
        FindingKind::PathNamesNoDocument,
        FindingKind::BodyBytesNotUtf8,
        FindingKind::FrontmatterTooLarge,
        FindingKind::FrontmatterUnclosed,
        FindingKind::FrontmatterUnreadable,
    ];

    /// The kind as the string it is on the wire.
    pub const fn as_str(&self) -> &'static str {
        match self {
            FindingKind::PathBytesNotUtf8 => "document/path-bytes-not-utf8",
            FindingKind::PathNamesNoDocument => "document/path-names-no-document",
            FindingKind::BodyBytesNotUtf8 => "document/body-bytes-not-utf8",
            FindingKind::FrontmatterTooLarge => "document/frontmatter-too-large",
            FindingKind::FrontmatterUnclosed => "document/frontmatter-unclosed",
            FindingKind::FrontmatterUnreadable => "document/frontmatter-unreadable",
        }
    }

    /// Where this kind's findings may stand.
    ///
    /// The match carries no wildcard: a kind minted without an answer here does
    /// not compile, because a producer recording it and a reader reading it both
    /// need to know whether a document row at the subject is a contradiction or
    /// the subject itself.
    pub const fn scope(&self) -> FindingScope {
        match self {
            // Nothing is derivable: there is no identity to hold a row under, or
            // no text to read facts out of.
            FindingKind::PathBytesNotUtf8
            | FindingKind::PathNamesNoDocument
            | FindingKind::BodyBytesNotUtf8 => FindingScope::Place,
            // The document is derived and its frontmatter block was read by
            // nothing, so the finding stands beside the row it is about.
            FindingKind::FrontmatterTooLarge
            | FindingKind::FrontmatterUnclosed
            | FindingKind::FrontmatterUnreadable => FindingScope::Document,
        }
    }
}

impl fmt::Display for FindingKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A string that spells no kind the registry holds.
///
/// A kind nobody minted is read as a refusal rather than as a kind, the same
/// way the tagged vocabulary refuses a variant it does not know.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnknownFindingKind;

impl fmt::Display for UnknownFindingKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the string spells no finding kind")
    }
}

impl std::error::Error for UnknownFindingKind {}

impl TryFrom<&str> for FindingKind {
    type Error = UnknownFindingKind;

    /// The kind a wire string names, found by walking [`FindingKind::ALL`]
    /// against the strings [`FindingKind::as_str`] hands out: reading a kind
    /// back is the inverse of writing it.
    fn try_from(string: &str) -> Result<Self, Self::Error> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == string)
            .ok_or(UnknownFindingKind)
    }
}
