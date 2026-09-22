//! `describe`: what a vault declares about itself, and what its documents
//! carry.
//!
//! **[`FieldType`] is a copy of the config crate's enum, and the copy is
//! deliberate.** The content model is `norn-config`'s, and this crate depends
//! on nothing in the workspace, so the declared type a facet reports is
//! spelled again here rather than reached for. Neither definition is derived
//! from the other; a test in `norn-config` walks both lists and holds them
//! equal, so a type added on one side without the other fails there rather
//! than crossing the seam as a spelling no reader has.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::address::VaultAddress;
use crate::cursor::{Cursor, FacetKind, Page};

/// The type a vault's schema declares a field under.
///
/// On the wire a type is the flat string itself: `"text"`, `"date"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FieldType {
    /// Any string, which is also what an undeclared field is read as.
    Text,
    /// A number, ordered numerically.
    Number,
    /// `true` or `false`.
    Boolean,
    /// A calendar day or an instant, ordered chronologically.
    Date,
    /// A set of tag names.
    Tags,
}

impl FieldType {
    /// Every type the vocabulary holds, in declaration order.
    pub const ALL: [FieldType; 5] = [
        FieldType::Text,
        FieldType::Number,
        FieldType::Boolean,
        FieldType::Date,
        FieldType::Tags,
    ];

    /// The type as the string it is on the wire, which is the string a vault's
    /// schema declares it as.
    pub const fn as_str(&self) -> &'static str {
        match self {
            FieldType::Text => "text",
            FieldType::Number => "number",
            FieldType::Boolean => "boolean",
            FieldType::Date => "date",
            FieldType::Tags => "tags",
        }
    }
}

/// What container an observed field's value sits in.
///
/// On the wire a container is the flat string itself: `"scalar"`,
/// `"sequence"`, `"map"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContainerKind {
    /// One value.
    Scalar,
    /// A sequence of values.
    Sequence,
    /// A mapping.
    Map,
}

/// Which rule a path rule states.
///
/// On the wire a rule is the flat string itself: `"ambiguity_ignore"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PathRuleKind {
    /// Paths the resolution ladder does not count as candidates.
    AmbiguityIgnore,
}

/// One thing a vault says about itself, or one thing its documents say.
///
/// On the wire a facet is an object tagged `facet`:
/// `{"facet":"declared_tag","name":"area"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "facet", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Facet {
    /// A field the vault's schema declares, with the declaration.
    #[non_exhaustive]
    DeclaredField {
        /// The frontmatter key.
        key: String,
        /// The type the declaration gives it.
        field_type: FieldType,
        /// Whether every document is declared to carry it.
        required: bool,
        /// The closed set of values it is declared to hold, and `null` where
        /// it is not declared closed.
        one_of: Option<Vec<String>>,
    },
    /// A field the vault's documents carry, declared or not.
    #[non_exhaustive]
    ObservedField {
        /// The frontmatter key.
        key: String,
        /// What container its values sit in.
        container: ContainerKind,
    },
    /// A tag name the vault's schema declares.
    #[non_exhaustive]
    DeclaredTag {
        /// The tag, without its `#`.
        name: String,
    },
    /// A pattern the vault's tag facet admits beyond its literal names.
    #[non_exhaustive]
    TagPattern {
        /// The pattern, as the schema writes it.
        pattern: String,
    },
    /// A folder the vault's schema declares.
    #[non_exhaustive]
    Folder {
        /// The vault-root-relative path the folder is at.
        path: String,
        /// What the schema says the folder is for.
        description: Option<String>,
    },
    /// A path rule the vault's schema states.
    #[non_exhaustive]
    PathRule {
        /// Which rule it states.
        rule: PathRuleKind,
        /// The pattern it states it over.
        pattern: String,
    },
}

impl Facet {
    /// The field `key`, declared as `field_type`.
    pub fn declared_field(
        key: impl Into<String>,
        field_type: FieldType,
        required: bool,
        one_of: Option<Vec<String>>,
    ) -> Self {
        Facet::DeclaredField {
            key: key.into(),
            field_type,
            required,
            one_of,
        }
    }

    /// The field `key`, observed in `container`.
    pub fn observed_field(key: impl Into<String>, container: ContainerKind) -> Self {
        Facet::ObservedField {
            key: key.into(),
            container,
        }
    }

    /// The declared tag `name`.
    pub fn declared_tag(name: impl Into<String>) -> Self {
        Facet::DeclaredTag { name: name.into() }
    }

    /// The tag pattern `pattern`.
    pub fn tag_pattern(pattern: impl Into<String>) -> Self {
        Facet::TagPattern {
            pattern: pattern.into(),
        }
    }

    /// The folder at `path`.
    pub fn folder(path: impl Into<String>, description: Option<String>) -> Self {
        Facet::Folder {
            path: path.into(),
            description,
        }
    }

    /// The `rule` stated over `pattern`.
    pub fn path_rule(rule: PathRuleKind, pattern: impl Into<String>) -> Self {
        Facet::PathRule {
            rule,
            pattern: pattern.into(),
        }
    }

    /// What this facet is a facet of.
    ///
    /// The match carries no wildcard, so a facet minted without a kind does not
    /// compile: [`FacetKind`] is what a request selects facets by and what a
    /// facet cursor orders by, and this is the one place the two lists are held
    /// together. A tag pattern reports the declared-tag kind, because a pattern
    /// is part of what the vault declares its tag vocabulary to be.
    pub const fn kind(&self) -> FacetKind {
        match self {
            Facet::DeclaredField { .. } => FacetKind::DeclaredField,
            Facet::ObservedField { .. } => FacetKind::ObservedField,
            Facet::DeclaredTag { .. } | Facet::TagPattern { .. } => FacetKind::DeclaredTag,
            Facet::Folder { .. } => FacetKind::Folder,
            Facet::PathRule { .. } => FacetKind::PathRule,
        }
    }
}

/// What `describe` answers with: one page of facets.
pub type DescribeReport = Page<Facet>;

/// What a `describe` request carries.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct DescribeParams {
    /// The vault to answer about.
    pub vault: VaultAddress,
    /// The kinds of facet to report. Empty reports every kind.
    pub facets: Vec<FacetKind>,
    /// How many facets at most. `null` leaves the ceiling to the host.
    pub limit: Option<u32>,
    /// Where to continue from. `null` starts at the first facet.
    pub after: Option<Cursor>,
}

impl DescribeParams {
    /// A `describe` of `vault`: every facet of every kind.
    pub const fn new(vault: VaultAddress) -> Self {
        DescribeParams {
            vault,
            facets: Vec::new(),
            limit: None,
            after: None,
        }
    }

    /// The request reporting `facets` alone.
    #[must_use]
    pub fn with_facets(mut self, facets: impl IntoIterator<Item = FacetKind>) -> Self {
        self.facets = facets.into_iter().collect();
        self
    }

    /// The request bounded at `limit` facets.
    #[must_use]
    pub const fn with_limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// The request continuing from `after`.
    #[must_use]
    pub fn with_after(mut self, after: Cursor) -> Self {
        self.after = Some(after);
        self
    }
}
