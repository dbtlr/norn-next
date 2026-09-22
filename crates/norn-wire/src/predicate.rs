//! What a request says a document must satisfy.
//!
//! **A request carries a list, and the list is a conjunction.** There is no
//! `or` and no nesting: a document matches when it satisfies every part. One
//! flat shape is what makes a request renderable as repeated CLI flags and as
//! one JSON array alike, and what keeps the store's translation a fold over
//! parts rather than a tree walk.
//!
//! **A value is the text a document carries.** A frontmatter value crosses
//! here as it is written, and whether two values compare as numbers, as dates
//! or as strings is decided behind the vault's content model in the store,
//! against the field's declared type. Spelling a typed value here would make
//! this crate the second place the content model lives.
//!
//! **A match query is carried verbatim.** The full-text syntax belongs to the
//! engine that answers it, and this layer neither parses nor rewrites it. It
//! is a filter and never an order: what a ranked answer is ordered by is the
//! request's own order, not the presence of a match part.
//!
//! **A surface's sugar is the surface's.** A flag such as `--type note` is a
//! rendering that a client expands into an equality part before the request is
//! built; nothing here holds the sugar, so two surfaces cannot expand it two
//! ways.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::finding::FindingKind;
use crate::target::ResolutionTarget;

/// One part of the conjunction a request filters by.
///
/// On the wire a part is an object tagged `op`:
/// `{"op":"eq","key":"type","value":"note"}`, `{"op":"tag","name":"draft"}`. A
/// request carries a list of them, and a document matches when it satisfies
/// every one.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Predicate {
    /// The document's value for `key` is `value`.
    #[non_exhaustive]
    Eq {
        /// The frontmatter key.
        key: String,
        /// The value the key must hold, as text.
        value: String,
    },
    /// The document's value for `key` is not `value`.
    #[non_exhaustive]
    NotEq {
        /// The frontmatter key.
        key: String,
        /// The value the key must not hold, as text.
        value: String,
    },
    /// The document's value for `key` is one of `values`.
    #[non_exhaustive]
    In {
        /// The frontmatter key.
        key: String,
        /// The values the key may hold, as text.
        values: Vec<String>,
    },
    /// The document carries `key` at all.
    #[non_exhaustive]
    Has {
        /// The frontmatter key.
        key: String,
    },
    /// The document does not carry `key`.
    #[non_exhaustive]
    Missing {
        /// The frontmatter key.
        key: String,
    },
    /// The document's value for `key` sorts before `value`.
    #[non_exhaustive]
    Before {
        /// The frontmatter key.
        key: String,
        /// The bound, as text.
        value: String,
    },
    /// The document's value for `key` sorts after `value`.
    #[non_exhaustive]
    After {
        /// The frontmatter key.
        key: String,
        /// The bound, as text.
        value: String,
    },
    /// The document matches `query` in full text. This filters; it does not
    /// order.
    #[non_exhaustive]
    Matches {
        /// The full-text query, carried as written.
        query: String,
    },
    /// The document's path matches `glob`.
    #[non_exhaustive]
    Path {
        /// The glob the path must match.
        glob: String,
    },
    /// The document carries a link whose target is `target`.
    #[non_exhaustive]
    LinksTo {
        /// The link target.
        target: ResolutionTarget,
    },
    /// The document is what `target` resolves to. Meaningful on `find` alone;
    /// `count` and `validate` report it as an unsatisfied part.
    #[non_exhaustive]
    Resolves {
        /// The target being resolved.
        target: ResolutionTarget,
    },
    /// The document carries the tag `name`.
    #[non_exhaustive]
    Tag {
        /// The tag, without its `#`.
        name: String,
    },
    /// A finding of `kind` stands over the document.
    #[non_exhaustive]
    HasFinding {
        /// The finding kind.
        kind: FindingKind,
    },
}

impl Predicate {
    /// `key` holds `value`.
    pub fn eq(key: impl Into<String>, value: impl Into<String>) -> Self {
        Predicate::Eq {
            key: key.into(),
            value: value.into(),
        }
    }

    /// `key` does not hold `value`.
    pub fn not_eq(key: impl Into<String>, value: impl Into<String>) -> Self {
        Predicate::NotEq {
            key: key.into(),
            value: value.into(),
        }
    }

    /// `key` holds one of `values`.
    pub fn in_any(key: impl Into<String>, values: impl IntoIterator<Item = String>) -> Self {
        Predicate::In {
            key: key.into(),
            values: values.into_iter().collect(),
        }
    }

    /// `key` is present.
    pub fn has(key: impl Into<String>) -> Self {
        Predicate::Has { key: key.into() }
    }

    /// `key` is absent.
    pub fn missing(key: impl Into<String>) -> Self {
        Predicate::Missing { key: key.into() }
    }

    /// `key` sorts before `value`.
    pub fn before(key: impl Into<String>, value: impl Into<String>) -> Self {
        Predicate::Before {
            key: key.into(),
            value: value.into(),
        }
    }

    /// `key` sorts after `value`.
    pub fn after(key: impl Into<String>, value: impl Into<String>) -> Self {
        Predicate::After {
            key: key.into(),
            value: value.into(),
        }
    }

    /// The full text matches `query`.
    pub fn matches(query: impl Into<String>) -> Self {
        Predicate::Matches {
            query: query.into(),
        }
    }

    /// The path matches `glob`.
    pub fn path(glob: impl Into<String>) -> Self {
        Predicate::Path { glob: glob.into() }
    }

    /// A link to `target` stands in the document.
    pub const fn links_to(target: ResolutionTarget) -> Self {
        Predicate::LinksTo { target }
    }

    /// The document is what `target` resolves to.
    pub const fn resolves(target: ResolutionTarget) -> Self {
        Predicate::Resolves { target }
    }

    /// The tag `name` stands on the document.
    pub fn tag(name: impl Into<String>) -> Self {
        Predicate::Tag { name: name.into() }
    }

    /// A finding of `kind` stands over the document.
    pub const fn has_finding(kind: FindingKind) -> Self {
        Predicate::HasFinding { kind }
    }
}
