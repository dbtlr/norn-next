//! The repair advisories a repair plan's provenance carries: facts a repair
//! noticed about the vault's fields and schema while it planned, that no
//! single finding states.
//!
//! **A repair advisory is a record, never an instruction.** It names the vault's
//! fields, rules and a bounded sample of documents, and carries no operation:
//! applying the plan it rides on neither reads nor acts on it, as the rest of
//! [`Provenance`](crate::Provenance) is never checked at apply.
//!
//! **The slot is a closed state.** A plan's provenance carries
//! [`RepairAdvisories`] in one of three states: absent on a continuation
//! batch, `omitted` on the first batch of a repair a path or document
//! predicate narrowed, and `computed` on the first batch of an unnarrowed
//! repair. A computed list is truncated exactly when it holds fewer items than
//! its total says there were.
//!
//! **Every type here refuses what it does not know** and is built from the
//! bounded heads the rest of a repair plan uses, so a sample never exceeds
//! [`CANDIDATE_HEAD`](crate::CANDIDATE_HEAD) documents.

// Host::repair builds this catalogue on the first batch without document
// predicates, on the same snapshot as the findings. Continuations omit it,
// and a narrowed first batch records why it was omitted.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::document::TotalBelowHead;
use crate::finding_row::DocumentPathHead;
use crate::read::describe::ContainerKind;

/// The type a field's value holds in a document, as an advisory names it.
///
/// On the wire a type is the flat string itself: `"string"`, `"integer"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AdvisoryValueType {
    /// Text.
    String,
    /// A whole number.
    Integer,
    /// A number with a fraction.
    Real,
    /// A boolean.
    Boolean,
    /// A null.
    Null,
    /// A mapping of values by key.
    Map,
}

/// The way two schema rules conflict when combined, as a schema-hygiene
/// advisory names it.
///
/// On the wire a conflict is an object tagged `of`:
/// `{"of":"required_and_forbidden","field":"owner"}`,
/// `{"of":"empty_one_of","field":"status"}`, `{"of":"disjoint_placement"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "of", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum HygieneConflict {
    /// One rule requires a field and the other forbids it.
    #[non_exhaustive]
    RequiredAndForbidden {
        /// The field the two rules conflict on.
        field: String,
    },
    /// The rules close one field to sets that share no value.
    #[non_exhaustive]
    EmptyOneOf {
        /// The field the two rules conflict on.
        field: String,
    },
    /// The rules allow paths that share no document path.
    #[non_exhaustive]
    DisjointPlacement {},
}

impl HygieneConflict {
    /// One rule requires `field` and the other forbids it.
    pub fn required_and_forbidden(field: impl Into<String>) -> Self {
        HygieneConflict::RequiredAndForbidden {
            field: field.into(),
        }
    }

    /// The rules close `field` to sets that share no value.
    pub fn empty_one_of(field: impl Into<String>) -> Self {
        HygieneConflict::EmptyOneOf {
            field: field.into(),
        }
    }

    /// The rules allow paths that share no document path.
    pub const fn disjoint_placement() -> Self {
        HygieneConflict::DisjointPlacement {}
    }
}

/// One type a field holds, and how many documents hold it.
///
/// On the wire: `{"type":"string","documents":7}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct TypeUse {
    /// The type.
    #[serde(rename = "type")]
    pub value_type: AdvisoryValueType,
    /// How many documents hold the field with this type.
    pub documents: u64,
}

impl TypeUse {
    /// The type `value_type`, held by `documents` documents.
    pub const fn new(value_type: AdvisoryValueType, documents: u64) -> Self {
        TypeUse {
            value_type,
            documents,
        }
    }
}

/// One shape a field holds, and how many documents hold it.
///
/// On the wire: `{"shape":"sequence","documents":9}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ShapeUse {
    /// The shape: the container the store records the value in.
    pub shape: ContainerKind,
    /// How many documents hold the field with this shape.
    pub documents: u64,
}

impl ShapeUse {
    /// The shape `shape`, held by `documents` documents.
    pub const fn new(shape: ContainerKind, documents: u64) -> Self {
        ShapeUse { shape, documents }
    }
}

/// One spelling of a field, and how many documents spell it so.
///
/// On the wire: `{"key":"Due","documents":2}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct SpellingUse {
    /// The frontmatter key as the documents spell it.
    pub key: String,
    /// How many documents spell the key so.
    pub documents: u64,
}

impl SpellingUse {
    /// The spelling `key`, used by `documents` documents.
    pub fn new(key: impl Into<String>, documents: u64) -> Self {
        SpellingUse {
            key: key.into(),
            documents,
        }
    }
}

/// A fact about the schema itself that a repair noticed, in one of three
/// cases.
///
/// On the wire a case is an object tagged `case`:
/// `{"case":"rule_matches_nothing","rule":"stale"}`,
/// `{"case":"unused_one_of_value","rule":"status","field":"status","value":"someday"}`,
/// `{"case":"empty_combination","rules":["alpha","beta"],"conflict":{"of":"empty_one_of","field":"status"}}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "case", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum SchemaHygiene {
    /// A rule whose selector matches no document in the vault.
    #[non_exhaustive]
    RuleMatchesNothing {
        /// The rule's name.
        rule: String,
    },
    /// A value a rule's closed set admits that no document holds.
    #[non_exhaustive]
    UnusedOneOfValue {
        /// The rule's name.
        rule: String,
        /// The field the closed set constrains.
        field: String,
        /// The member no document holds.
        value: String,
    },
    /// Two rules that select a common document and together admit no
    /// document at all.
    #[non_exhaustive]
    EmptyCombination {
        /// The two rules, in name order; a writer sorts them, and a reader
        /// does not check it.
        rules: [String; 2],
        /// How they conflict.
        conflict: HygieneConflict,
    },
}

impl SchemaHygiene {
    /// The rule `rule` selects no document.
    pub fn rule_matches_nothing(rule: impl Into<String>) -> Self {
        SchemaHygiene::RuleMatchesNothing { rule: rule.into() }
    }

    /// The closed set `rule` declares over `field` admits `value`, which no
    /// document holds.
    pub fn unused_one_of_value(
        rule: impl Into<String>,
        field: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        SchemaHygiene::UnusedOneOfValue {
            rule: rule.into(),
            field: field.into(),
            value: value.into(),
        }
    }

    /// The two `rules` admit no document together, for the reason `conflict`.
    /// The names are sorted here, so a combination is written in name order
    /// whichever order they are given in; the read does not enforce it.
    pub fn empty_combination(mut rules: [String; 2], conflict: HygieneConflict) -> Self {
        rules.sort();
        SchemaHygiene::EmptyCombination { rules, conflict }
    }
}

/// One advisory a repair computed.
///
/// On the wire an advisory is an object tagged `kind`, and no kind carries an
/// operation. A `sample` is a bounded head of the documents concerned, and a
/// `yaml` is a ready-to-paste fragment of schema configuration:
/// `{"kind":"mixed_types","key":"priority","types":[{"type":"string","documents":7},{"type":"integer","documents":3}],"sample":{"paths":["a.md"],"total":10}}`,
/// `{"kind":"undeclared_uniform","key":"owner","type":"boolean","documents":12,"sample":{…},"yaml":"…"}`,
/// `{"kind":"schema_hygiene","hygiene":{"case":"rule_matches_nothing","rule":"stale"}}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum RepairAdvisory {
    /// A field that documents hold as more than one type.
    #[non_exhaustive]
    MixedTypes {
        /// The frontmatter key.
        key: String,
        /// Each type the field holds, with its document count.
        types: Vec<TypeUse>,
        /// A bounded sample of the documents holding the field.
        sample: DocumentPathHead,
    },
    /// A field that documents hold in more than one shape.
    #[non_exhaustive]
    MixedShapes {
        /// The frontmatter key.
        key: String,
        /// Each shape the field holds, with its document count.
        shapes: Vec<ShapeUse>,
        /// A bounded sample of the documents holding the field.
        sample: DocumentPathHead,
    },
    /// A field every document holds as one type that no schema rule declares.
    #[non_exhaustive]
    UndeclaredUniform {
        /// The frontmatter key.
        key: String,
        /// The one type the documents hold it as.
        #[serde(rename = "type")]
        value_type: AdvisoryValueType,
        /// How many documents hold the field.
        documents: u64,
        /// A bounded sample of the documents holding the field.
        sample: DocumentPathHead,
        /// A fragment of schema configuration that would declare the field.
        yaml: String,
    },
    /// Spellings of one field that differ only in case or separators.
    #[non_exhaustive]
    NearDuplicateFields {
        /// Each spelling, with its document count.
        spellings: Vec<SpellingUse>,
        /// A bounded sample of the documents spelling the field any way.
        sample: DocumentPathHead,
        /// A fragment of configuration that would settle on one spelling.
        yaml: String,
    },
    /// A fact about the schema itself rather than about the documents.
    #[non_exhaustive]
    SchemaHygiene {
        /// Which fact, and what it names.
        hygiene: SchemaHygiene,
    },
}

impl RepairAdvisory {
    /// The field `key`, held as each of `types`.
    pub fn mixed_types(
        key: impl Into<String>,
        types: Vec<TypeUse>,
        sample: DocumentPathHead,
    ) -> Self {
        RepairAdvisory::MixedTypes {
            key: key.into(),
            types,
            sample,
        }
    }

    /// The field `key`, held in each of `shapes`.
    pub fn mixed_shapes(
        key: impl Into<String>,
        shapes: Vec<ShapeUse>,
        sample: DocumentPathHead,
    ) -> Self {
        RepairAdvisory::MixedShapes {
            key: key.into(),
            shapes,
            sample,
        }
    }

    /// The undeclared field `key`, held by `documents` documents all as
    /// `value_type`, with `yaml` that would declare it.
    pub fn undeclared_uniform(
        key: impl Into<String>,
        value_type: AdvisoryValueType,
        documents: u64,
        sample: DocumentPathHead,
        yaml: impl Into<String>,
    ) -> Self {
        RepairAdvisory::UndeclaredUniform {
            key: key.into(),
            value_type,
            documents,
            sample,
            yaml: yaml.into(),
        }
    }

    /// The `spellings` of one field, with `yaml` that would settle on one.
    pub fn near_duplicate_fields(
        spellings: Vec<SpellingUse>,
        sample: DocumentPathHead,
        yaml: impl Into<String>,
    ) -> Self {
        RepairAdvisory::NearDuplicateFields {
            spellings,
            sample,
            yaml: yaml.into(),
        }
    }

    /// The schema fact `hygiene`.
    pub const fn schema_hygiene(hygiene: SchemaHygiene) -> Self {
        RepairAdvisory::SchemaHygiene { hygiene }
    }
}

/// Why a repair carries no advisories.
///
/// On the wire the reason is the flat string itself: `"narrowed"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AdvisoriesOmitted {
    /// A path or document predicate narrowed the repair, and advisories
    /// describe the whole vault, so the first batch does not compute them.
    Narrowed,
}

/// The advisories the first batch of an unnarrowed repair computed: the items
/// and how many there were.
///
/// On the wire: `{"items":[…],"total":3}`. The list is truncated exactly when
/// it holds fewer items than `total`. A total below the items beside it
/// heads nothing, and the read refuses it.
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ComputedAdvisories {
    /// The advisories this batch carries.
    items: Vec<RepairAdvisory>,
    /// How many advisories the repair computed, which is what makes the items
    /// a head.
    total: u64,
}

impl ComputedAdvisories {
    /// The `items` that fit, out of `total` the repair computed, or the
    /// reason `total` heads nothing.
    pub fn new(
        items: impl IntoIterator<Item = RepairAdvisory>,
        total: u64,
    ) -> Result<Self, TotalBelowHead> {
        let items: Vec<RepairAdvisory> = items.into_iter().collect();
        TotalBelowHead::check(items.len(), total)?;
        Ok(ComputedAdvisories { items, total })
    }

    /// The advisories carried.
    pub fn items(&self) -> &[RepairAdvisory] {
        &self.items
    }

    /// How many advisories the repair computed.
    pub const fn total(&self) -> u64 {
        self.total
    }

    /// Whether the repair computed more advisories than this carries.
    pub fn is_truncated(&self) -> bool {
        (self.items.len() as u64) < self.total
    }
}

impl<'de> Deserialize<'de> for ComputedAdvisories {
    /// The list arrives as its items and its total and is read back through
    /// the check the constructor holds: a total below the items beside it
    /// heads nothing, so it refuses the read.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            items: Vec<RepairAdvisory>,
            total: u64,
        }
        let fields = Fields::deserialize(deserializer)?;
        ComputedAdvisories::new(fields.items, fields.total).map_err(D::Error::custom)
    }
}

/// The advisories slot of a repair plan's provenance, when it is present.
///
/// On the wire a state is an object with one key. The slot itself is left
/// out of a continuation batch's provenance:
/// `{"omitted":"narrowed"}` on the first batch of a repair a path or
/// document predicate narrowed, and `{"computed":{"items":[…],"total":3}}` on
/// the first batch of an unnarrowed repair.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum RepairAdvisories {
    /// The repair computed no advisories, for the reason given.
    Omitted(AdvisoriesOmitted),
    /// The repair computed these advisories.
    Computed(ComputedAdvisories),
}

impl RepairAdvisories {
    /// No advisories, for `reason`.
    pub const fn omitted(reason: AdvisoriesOmitted) -> Self {
        RepairAdvisories::Omitted(reason)
    }

    /// No advisories, because a path or document predicate narrowed the
    /// repair.
    pub const fn omitted_narrowed() -> Self {
        RepairAdvisories::Omitted(AdvisoriesOmitted::Narrowed)
    }

    /// The `items` that fit, out of `total` the repair computed, or the
    /// reason `total` heads nothing.
    pub fn computed(
        items: impl IntoIterator<Item = RepairAdvisory>,
        total: u64,
    ) -> Result<Self, TotalBelowHead> {
        ComputedAdvisories::new(items, total).map(RepairAdvisories::Computed)
    }
}
