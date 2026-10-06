//! What a finding is filed under and how urgently it is reported, as two
//! closed lists.
//!
//! A finding kind is the cause class a reader dispatches on, and it is a code
//! in the same grammar refusals use — so the kind a store row carries, the
//! kind a report renders and the kind a client filters by are one string with
//! one spelling. Derivation records kinds; it does not name them, and a kind
//! nobody can enumerate here is a kind no surface can advertise.
//!
//! The set holds three namespaces. `document/…` is a fact about one document:
//! a vault holding a document norn cannot fully read stays serviceable, and the
//! finding is where what is missing from derived state is stated. A kind may
//! also state that a document derived *whole* and disagrees with what the vault
//! declares about itself, which is what the vault schema's content model
//! judges — an undeclared tag, or a document standing where the rules
//! selecting it do not allow. `field/…` is a fact about one frontmatter field a
//! document derived whole holds or lacks, judged against the type and shape
//! the schema declares the field with or against the combined constraint of
//! the schema rules selecting the document, per ADR 0035; the field is the
//! finding's `target`. `link/…` is a fact about one link a document holds —
//! broken, ambiguous, or missing the heading or block its target names — per
//! ADR 0027; the document it stands in still derives whole.
//!
//! **Rule judgment in derivation files the schema-judged kinds.** The nine
//! kinds — the seven `field/…` kinds, `document/misplaced` and
//! `document/rules-conflict` — are concluded per document, in the document's
//! own changeset, from its path and frontmatter and the vault schema.
//!
//! **A rule kind's severity is its rules'.** A finding judged against the
//! schema rules cites every rule contributing to the constraint it breaches
//! and is reported at the highest of their severities, so the severity a
//! producer files it at is read off those rules; a rule that states none is a
//! warning, which is what [`FindingKind::default_severity`] says of each rule
//! kind. A type or shape mismatch is judged against the field declarations,
//! which state no severity and no rule, and is always a warning.
//!
//! **A kind also says where its findings may stand**, as [`FindingScope`]. A
//! kind whose cause leaves nothing derivable is about the *place* and stands
//! only where no document row does; a kind whose cause leaves the document
//! derivable but incomplete is about the *document* and stands beside its row.
//! A client reading the findings at a path branches on that: a place-scoped
//! finding there says no document is derived at it, and a document-scoped one
//! says the document beside it is derived with something missing.
//!
//! [`FindingScope`] is plain rather than `#[non_exhaustive]`: a producer or a
//! reader that has not decided what a new scope means should fail to compile
//! rather than fall into a default arm. The rationale sits here because a doc
//! comment on the type is published as the schema `description` an MCP
//! consumer reads, and what governs Rust destructuring is not something that
//! consumer can see or write.

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

    /// Whether this severity is reported at `floor` or more urgently: an error
    /// is at least a warning, and a warning is not at least an error.
    pub const fn is_at_least(self, floor: Severity) -> bool {
        self.urgency() >= floor.urgency()
    }

    /// Where the severity stands in urgency, the least urgent lowest.
    const fn urgency(self) -> u8 {
        match self {
            Severity::Warning => 0,
            Severity::Error => 1,
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
    /// `document/undeclared-tag` — the document carries a tag the vault's
    /// declared tag facet does not admit. The document derives whole; what the
    /// finding states is that the vault's own vocabulary does not hold this
    /// name. The tag is the finding's `target`.
    #[serde(rename = "document/undeclared-tag")]
    UndeclaredTag,
    /// `link/broken` — a link in the document names no document. The document
    /// derives whole; the link is the finding's `target`.
    #[serde(rename = "link/broken")]
    Broken,
    /// `link/ambiguous` — a link in the document names two or more documents.
    /// The document derives whole; the link is the finding's `target`, and its
    /// row carries a bounded head of the candidates it resolves to.
    #[serde(rename = "link/ambiguous")]
    Ambiguous,
    /// `link/missing-anchor` — a link in the document names one document, but
    /// that document lacks the link's `#heading` or `#^block`. The document
    /// derives whole; the link is the finding's `target`.
    #[serde(rename = "link/missing-anchor")]
    MissingAnchor,
    /// `document/misplaced` — the document stands at a path the rules
    /// selecting it do not allow: a document stands only where every rule
    /// stating allowed paths allows it. The finding cites every rule stating
    /// allowed paths that selects the document. It names no field and no
    /// value.
    #[serde(rename = "document/misplaced")]
    Misplaced,
    /// `document/rules-conflict` — the rules selecting the document state
    /// allowed paths admitting no document path in common — one rule's alone
    /// included — so no place satisfies them all. One finding per document
    /// whatever path it stands at, citing every rule stating allowed paths
    /// that selects it. It names no field and no value.
    #[serde(rename = "document/rules-conflict")]
    DocumentRulesConflict,
    /// `field/required-missing` — a field some rule selecting the document
    /// requires is absent or null. The field is the finding's `target`; it
    /// names no value, and it cites every rule requiring the field.
    #[serde(rename = "field/required-missing")]
    RequiredMissing,
    /// `field/forbidden` — the document holds a field some rule selecting it
    /// forbids, null included. The field is the finding's `target`, its value
    /// the finding's value — none where it holds null, which is no value —
    /// and it cites every rule forbidding the field.
    #[serde(rename = "field/forbidden")]
    Forbidden,
    /// `field/not-one-of` — a value the field holds, or an element of the list
    /// it holds, is outside the intersection of the closed sets the rules
    /// selecting the document state for it. One finding per distinct
    /// offending value as the closed sets compare it — a tag key's tag under
    /// the tag fold, a typed key's typed value — whose spelling written first
    /// is the finding's value; the field is its `target`, and it cites every
    /// rule closing the field.
    #[serde(rename = "field/not-one-of")]
    NotOneOf,
    /// `field/too-long` — a value the field holds, or an element of the list
    /// it holds, is longer than the smallest length limit the rules selecting
    /// the document state for it. One finding per distinct offending value as
    /// a closed set would compare it, whose first spelling past the limit is
    /// the finding's value; the field is its `target`, and it cites every rule
    /// limiting the field.
    #[serde(rename = "field/too-long")]
    TooLong,
    /// `field/type-mismatch` — a value the field holds, or an element of the
    /// list it holds, does not read as the type the schema declares the field
    /// with. One finding per distinct offending value, which is the finding's
    /// value; the field is its `target`. It is judged against the field
    /// declarations, so it cites no rule, and it is always a warning.
    #[serde(rename = "field/type-mismatch")]
    TypeMismatch,
    /// `field/shape-mismatch` — the field holds a list where the schema
    /// declares it single, or one value where it declares it a list. The value
    /// held is the finding's value and the field its `target`. It is judged
    /// against the field declarations, so it cites no rule, and it is always a
    /// warning.
    #[serde(rename = "field/shape-mismatch")]
    ShapeMismatch,
    /// `field/rules-conflict` — the rules selecting the document constrain the
    /// field in ways nothing satisfies: one requires it and another forbids
    /// it, or their closed sets share no member where the field is required or
    /// holds a value they judge. One finding per field whatever value it
    /// holds: the value is the finding's payload, not part of what makes two
    /// conflicts one, so the write gate judging a conflict (NORN-359) compares
    /// it by field. Where the field holds a value, that whole value — its
    /// canonical JSON for a list or a map — is the finding's value; where the
    /// field is absent or null the finding names none. The field is its
    /// `target`, and it cites every rule contributing to the conflict. A
    /// required field's conflict stands instead of its required, forbidden
    /// and closed-set findings; closed sets sharing no member on a field no
    /// rule requires stand instead of its closed-set findings alone, beside
    /// any forbidden finding.
    #[serde(rename = "field/rules-conflict")]
    FieldRulesConflict,
}

/// Where the findings of a kind may stand.
///
/// The two scopes differ in one thing: whether a document row at the subject
/// withholds the finding. Everything else — how a finding is recorded, read,
/// filtered and discarded — is the same for both.
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
    pub const ALL: [FindingKind; 19] = [
        FindingKind::PathBytesNotUtf8,
        FindingKind::PathNamesNoDocument,
        FindingKind::BodyBytesNotUtf8,
        FindingKind::FrontmatterTooLarge,
        FindingKind::FrontmatterUnclosed,
        FindingKind::FrontmatterUnreadable,
        FindingKind::UndeclaredTag,
        FindingKind::Broken,
        FindingKind::Ambiguous,
        FindingKind::MissingAnchor,
        FindingKind::Misplaced,
        FindingKind::DocumentRulesConflict,
        FindingKind::RequiredMissing,
        FindingKind::Forbidden,
        FindingKind::NotOneOf,
        FindingKind::TooLong,
        FindingKind::TypeMismatch,
        FindingKind::ShapeMismatch,
        FindingKind::FieldRulesConflict,
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
            FindingKind::UndeclaredTag => "document/undeclared-tag",
            FindingKind::Broken => "link/broken",
            FindingKind::Ambiguous => "link/ambiguous",
            FindingKind::MissingAnchor => "link/missing-anchor",
            FindingKind::Misplaced => "document/misplaced",
            FindingKind::DocumentRulesConflict => "document/rules-conflict",
            FindingKind::RequiredMissing => "field/required-missing",
            FindingKind::Forbidden => "field/forbidden",
            FindingKind::NotOneOf => "field/not-one-of",
            FindingKind::TooLong => "field/too-long",
            FindingKind::TypeMismatch => "field/type-mismatch",
            FindingKind::ShapeMismatch => "field/shape-mismatch",
            FindingKind::FieldRulesConflict => "field/rules-conflict",
        }
    }

    /// The severity a producer files this kind at when it holds no severity of
    /// its own to state: warning for every link-health kind, per ADR 0027;
    /// warning for every kind judged against the schema rules, which is the
    /// severity a rule stating none is reported at — a finding citing rules
    /// that state one is reported at the highest of theirs instead, per ADR
    /// 0035; warning for a type or shape mismatch, which no rule states a
    /// severity for; and otherwise the one severity every current producer of
    /// that kind uses.
    pub const fn default_severity(&self) -> Severity {
        match self {
            // Nothing is derivable, or the document's frontmatter block was
            // read by nothing: the document's own facts are the ones missing.
            FindingKind::PathBytesNotUtf8
            | FindingKind::PathNamesNoDocument
            | FindingKind::BodyBytesNotUtf8
            | FindingKind::FrontmatterTooLarge
            | FindingKind::FrontmatterUnclosed
            | FindingKind::FrontmatterUnreadable => Severity::Error,
            // The document derives whole; what the finding states is a
            // judgment about one fact on its row, at a severity that deserves
            // attention rather than correction.
            FindingKind::UndeclaredTag
            | FindingKind::Broken
            | FindingKind::Ambiguous
            | FindingKind::MissingAnchor => Severity::Warning,
            // Judged against the rules selecting the document: the floor a
            // rule stating no severity is reported at.
            FindingKind::Misplaced
            | FindingKind::DocumentRulesConflict
            | FindingKind::RequiredMissing
            | FindingKind::Forbidden
            | FindingKind::NotOneOf
            | FindingKind::TooLong
            | FindingKind::FieldRulesConflict => Severity::Warning,
            // Judged against the field declarations, which state no severity.
            FindingKind::TypeMismatch | FindingKind::ShapeMismatch => Severity::Warning,
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
            | FindingKind::FrontmatterUnreadable
            // The document is derived whole, and what the finding states is a
            // judgment about one of the facts on its row.
            | FindingKind::UndeclaredTag
            // A link-health finding is a judgment about one link the document
            // holds; the document itself derives whole regardless of what its
            // links resolve to.
            | FindingKind::Broken
            | FindingKind::Ambiguous
            | FindingKind::MissingAnchor
            // A schema judgment reads a document that derived whole — its path
            // and its frontmatter — so the finding stands beside the row it
            // judged.
            | FindingKind::Misplaced
            | FindingKind::DocumentRulesConflict
            | FindingKind::RequiredMissing
            | FindingKind::Forbidden
            | FindingKind::NotOneOf
            | FindingKind::TooLong
            | FindingKind::TypeMismatch
            | FindingKind::ShapeMismatch
            | FindingKind::FieldRulesConflict => FindingScope::Document,
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
