//! `describe`: what a vault declares about itself, and what its documents
//! carry.
//!
//! **[`FieldType`] and [`TagStance`] are copies of the config crate's enums,
//! and the copies are deliberate.** The content model is `norn-config`'s, and
//! this crate depends on nothing in the workspace, so the declared type a
//! facet reports and the stance it reports on undeclared tags are spelled
//! again here rather than reached for. Neither definition is derived from the
//! other; a test in `norn-config` walks each pair of lists and holds them
//! equal, so a member added on one side without the other fails there rather
//! than crossing the seam as a spelling no reader has.
//!
//! **A facet's kind is injective.** [`Facet::kind`] maps each variant to a
//! distinct [`FacetKind`], so `--facets` selects exactly one shape and a
//! facet cursor orders one shape at a time. Two variants sharing a kind would
//! make a selection ambiguous and an ordered page interleave two row shapes
//! under one key.
//!
//! **A facet's cursor key is the facet's own key.** [`Facet::cursor_key`] is
//! the one function that turns a facet into the position a page stops at, and
//! it states that position for every shape, so the seven keys cannot drift
//! into two orders sharing a kind. What each shape is keyed by is stated
//! there rather than restated here.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::address::VaultAddress;
use crate::cursor::{Cursor, CursorKey, FacetKind, Page};

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

/// What the vault says about a tag its facet does not admit.
///
/// On the wire a stance is the flat string itself: `"allow"`, `"report"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TagStance {
    /// Anything may be tagged.
    Allow,
    /// A tag outside the vocabulary is a finding.
    Report,
}

impl TagStance {
    /// Every stance the vocabulary holds, in declaration order.
    pub const ALL: [TagStance; 2] = [TagStance::Allow, TagStance::Report];

    /// The stance as the string it is on the wire, which is the string a
    /// vault's schema declares it as.
    pub const fn as_str(&self) -> &'static str {
        match self {
            TagStance::Allow => "allow",
            TagStance::Report => "report",
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

impl ContainerKind {
    /// Every container the vocabulary holds, in declaration order, which is
    /// the order an observed field lists the containers it is held in.
    pub const ALL: [ContainerKind; 3] = [
        ContainerKind::Scalar,
        ContainerKind::Sequence,
        ContainerKind::Map,
    ];

    /// Where this container stands in [`ContainerKind::ALL`].
    const fn position(self) -> usize {
        match self {
            ContainerKind::Scalar => 0,
            ContainerKind::Sequence => 1,
            ContainerKind::Map => 2,
        }
    }
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
    ///
    /// One facet per key: a key one document holds as a scalar and another as
    /// a sequence is one observed field held in both.
    #[non_exhaustive]
    ObservedField {
        /// The frontmatter key.
        key: String,
        /// Every container some document holds the key's value in, each once,
        /// in the order the container vocabulary declares: scalar, sequence,
        /// map.
        containers: Vec<ContainerKind>,
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
    /// What the vault's schema says about a tag its facet does not admit.
    #[non_exhaustive]
    UndeclaredTags {
        /// The stance the schema declares.
        stance: TagStance,
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

    /// The field `key`, observed held in each of `containers`: listed each
    /// once, in [`ContainerKind::ALL`]'s order, whatever order they are named
    /// in.
    pub fn observed_field(
        key: impl Into<String>,
        containers: impl IntoIterator<Item = ContainerKind>,
    ) -> Self {
        let mut held = [false; ContainerKind::ALL.len()];
        for container in containers {
            held[container.position()] = true;
        }
        Facet::ObservedField {
            key: key.into(),
            containers: ContainerKind::ALL
                .into_iter()
                .filter(|container| held[container.position()])
                .collect(),
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

    /// The schema's `stance` on a tag its facet does not admit.
    pub const fn undeclared_tags(stance: TagStance) -> Self {
        Facet::UndeclaredTags { stance }
    }

    /// What this facet is a facet of.
    ///
    /// The match carries no wildcard, so a facet minted without a kind does not
    /// compile: [`FacetKind`] is what a request selects facets by and what a
    /// facet cursor orders by, and this is the one place the two lists are held
    /// together. The map is injective — one kind per shape — so a request
    /// naming a kind names one shape and a page ordered by kind holds one.
    pub const fn kind(&self) -> FacetKind {
        match self {
            Facet::DeclaredField { .. } => FacetKind::DeclaredField,
            Facet::ObservedField { .. } => FacetKind::ObservedField,
            Facet::DeclaredTag { .. } => FacetKind::DeclaredTag,
            Facet::TagPattern { .. } => FacetKind::TagPattern,
            Facet::Folder { .. } => FacetKind::Folder,
            Facet::PathRule { .. } => FacetKind::PathRule,
            Facet::UndeclaredTags { .. } => FacetKind::UndeclaredTags,
        }
    }

    /// Where a page of facets stops at this facet.
    ///
    /// The key is the one text the facet itself spells: a declared field's
    /// and an observed field's frontmatter key, a declared tag's name, a tag
    /// pattern's and a path rule's pattern, a folder's path, and, for the
    /// undeclared-tags facet, the stance spelling — `allow` or `report`.
    /// Beside the kind, that names one facet within its shape, which is what a
    /// continuation resumes after.
    ///
    /// The destructuring carries no wildcard, so neither a facet shape minted
    /// without a key nor a field added to one compiles until this says what
    /// the order stops at.
    pub fn cursor_key(&self) -> CursorKey {
        let key = match self {
            Facet::DeclaredField {
                key,
                field_type: _,
                required: _,
                one_of: _,
            } => key.clone(),
            Facet::ObservedField { key, containers: _ } => key.clone(),
            Facet::DeclaredTag { name } => name.clone(),
            Facet::TagPattern { pattern } => pattern.clone(),
            Facet::Folder {
                path,
                description: _,
            } => path.clone(),
            Facet::PathRule { rule: _, pattern } => pattern.clone(),
            Facet::UndeclaredTags { stance } => stance.as_str().to_string(),
        };
        CursorKey::facet(self.kind(), key)
    }
}

/// What `describe` answers with: one page of facets, in `(kind, key)` order —
/// the kinds in the byte order of their codes ([`FacetKind::in_code_order`]),
/// and within a kind the facets in the byte order of their keys.
pub type DescribeReport = Page<Facet>;

/// What a `describe` request carries.
///
/// The facets it answers stand in `(kind, key)` order: the kind, in the byte
/// order of its code, then the key in byte order, which is the order a facet
/// cursor names a position in.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct DescribeParams {
    /// The vault to answer about.
    pub vault: VaultAddress,
    /// The kinds of facet to report. Empty reports every kind. Whatever order
    /// it names them in, they are answered in the byte order of their codes.
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
