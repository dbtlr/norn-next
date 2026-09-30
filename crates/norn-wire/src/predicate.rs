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
//! this crate the second place the content model lives. Unlike a field value,
//! a tag's name crosses as written and compares under the wire's tag fold,
//! [`crate::fold_tag`], not behind the content model.
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

use std::fmt;

use schemars::JsonSchema;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

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
    /// One of the values the document holds for `key` is `value`.
    #[non_exhaustive]
    Eq {
        /// The frontmatter key.
        key: String,
        /// The value the key must hold, as text.
        value: String,
    },
    /// No value the document holds for `key` is `value`.
    #[non_exhaustive]
    NotEq {
        /// The frontmatter key.
        key: String,
        /// The value the key must not hold, as text.
        value: String,
    },
    /// One of the values the document holds for `key` is one of `values`.
    #[non_exhaustive]
    In {
        /// The frontmatter key.
        key: String,
        /// The values the key may hold, as text: at least one.
        #[serde(deserialize_with = "at_least_one_value")]
        #[schemars(length(min = 1))]
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
    /// One of the values the document holds for `key` sorts before `value`.
    #[non_exhaustive]
    Before {
        /// The frontmatter key.
        key: String,
        /// The bound, as text.
        value: String,
    },
    /// One of the values the document holds for `key` sorts after `value`.
    #[non_exhaustive]
    After {
        /// The frontmatter key.
        key: String,
        /// The bound, as text.
        value: String,
    },
    /// The document matches `query` in full text. This filters; it does not
    /// order. The full-text index compares a word by its first 32768 bytes,
    /// in the index and in the query alike, so two words that share those
    /// bytes match each other.
    #[non_exhaustive]
    Matches {
        /// The full-text query, carried as written.
        query: String,
    },
    /// The document's path matches `glob`.
    #[non_exhaustive]
    Path {
        /// The glob the path must match, anchored at both ends: `?` matches
        /// one character that is not `/`, `*` any run of characters holding
        /// no `/`, and a whole `**` segment any run of segments, none
        /// included. Every other character matches itself, except that an
        /// ASCII letter matches either case of itself where the vault root's
        /// path order folds ASCII case; there is no escape.
        glob: String,
    },
    /// The document holds a link that resolves to exactly the one document
    /// `target` names: a link naming that document among others, or naming
    /// none, does not match. A `target` that names several documents or none
    /// matches no document, and is reported in band as an ambiguous or an
    /// unknown target.
    #[non_exhaustive]
    LinksTo {
        /// The target naming the one document the links resolve to.
        target: ResolutionTarget,
    },
    /// The document is what `target` resolves to. Meaningful on `find` alone;
    /// `count`, `validate` and `search` report it as an unsatisfied part.
    #[non_exhaustive]
    Resolves {
        /// The target being resolved.
        target: ResolutionTarget,
    },
    /// The document carries the tag `name`, with Unicode case folded and
    /// accents kept: `Work` finds `#work` and `#WORK`.
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
    ///
    /// Named apart from `PartialEq::eq`, which every wire type derives: two
    /// methods of one name on one type make `Predicate::eq(a, b)` read as a
    /// comparison of two predicates. The wire tag stays `eq`.
    pub fn equal_to(key: impl Into<String>, value: impl Into<String>) -> Self {
        Predicate::Eq {
            key: key.into(),
            value: value.into(),
        }
    }

    /// `key` does not hold `value`. The wire tag stays `not_eq`.
    pub fn not_equal_to(key: impl Into<String>, value: impl Into<String>) -> Self {
        Predicate::NotEq {
            key: key.into(),
            value: value.into(),
        }
    }

    /// `key` holds one of `values`.
    ///
    /// The read path refuses a membership naming no value; this constructor
    /// does not, and the store refuses one it is handed.
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

    /// The document holds a link that resolves to exactly the one document
    /// `target` names; a `target` naming several documents or none is
    /// reported in band and matches nothing.
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

/// A membership part's values as the read path takes them: at least one. A
/// membership in no value is a part no document satisfies, so bytes spelling
/// one are refused where they are read rather than carried to a store that
/// would refuse them later.
fn at_least_one_value<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<String>::deserialize(deserializer)?;
    if values.is_empty() {
        return Err(D::Error::custom(
            "a membership part names at least one value",
        ));
    }
    Ok(values)
}

/// A predicate in the wire's own names, for a person reading why a request
/// matched nothing: its `op` and its fields, each value quoted,
/// `{op: eq, key: "type", value: "note"}`. The wire carries it as JSON; this
/// is the same part in words, never a Rust type's.
impl fmt::Display for Predicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Predicate::Eq { key, value } => write!(f, "{{op: eq, key: {key:?}, value: {value:?}}}"),
            Predicate::NotEq { key, value } => {
                write!(f, "{{op: not_eq, key: {key:?}, value: {value:?}}}")
            }
            Predicate::In { key, values } => {
                write!(f, "{{op: in, key: {key:?}, values: ")?;
                quoted_list(f, values)?;
                f.write_str("}")
            }
            Predicate::Has { key } => write!(f, "{{op: has, key: {key:?}}}"),
            Predicate::Missing { key } => write!(f, "{{op: missing, key: {key:?}}}"),
            Predicate::Before { key, value } => {
                write!(f, "{{op: before, key: {key:?}, value: {value:?}}}")
            }
            Predicate::After { key, value } => {
                write!(f, "{{op: after, key: {key:?}, value: {value:?}}}")
            }
            Predicate::Matches { query } => write!(f, "{{op: matches, query: {query:?}}}"),
            Predicate::Path { glob } => write!(f, "{{op: path, glob: {glob:?}}}"),
            Predicate::LinksTo { target } => {
                write!(f, "{{op: links_to, target: {:?}}}", target.to_string())
            }
            Predicate::Resolves { target } => {
                write!(f, "{{op: resolves, target: {:?}}}", target.to_string())
            }
            Predicate::Tag { name } => write!(f, "{{op: tag, name: {name:?}}}"),
            Predicate::HasFinding { kind } => {
                write!(f, "{{op: has_finding, kind: {:?}}}", kind.as_str())
            }
        }
    }
}

/// `values` as a bracketed list, each quoted: `["a", "b"]`.
pub(crate) fn quoted_list(f: &mut fmt::Formatter<'_>, values: &[String]) -> fmt::Result {
    f.write_str("[")?;
    for (at, value) in values.iter().enumerate() {
        if at > 0 {
            f.write_str(", ")?;
        }
        write!(f, "{value:?}")?;
    }
    f.write_str("]")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A predicate reads in the wire's own names**: its `op` and its
    /// fields, each value quoted, and no Rust type name among them.
    #[test]
    fn a_predicate_reads_in_the_wires_own_names() {
        let target = ResolutionTarget::new("Plan").expect("a legal target");
        let spelled = [
            (
                Predicate::equal_to("wave", "flip"),
                r#"{op: eq, key: "wave", value: "flip"}"#,
            ),
            (
                Predicate::NotEq {
                    key: "wave".into(),
                    value: "flip".into(),
                },
                r#"{op: not_eq, key: "wave", value: "flip"}"#,
            ),
            (
                Predicate::In {
                    key: "wave".into(),
                    values: vec!["a".into(), "b\"c".into()],
                },
                r#"{op: in, key: "wave", values: ["a", "b\"c"]}"#,
            ),
            (Predicate::Has { key: "k".into() }, r#"{op: has, key: "k"}"#),
            (
                Predicate::Missing { key: "k".into() },
                r#"{op: missing, key: "k"}"#,
            ),
            (
                Predicate::Before {
                    key: "due".into(),
                    value: "2026".into(),
                },
                r#"{op: before, key: "due", value: "2026"}"#,
            ),
            (
                Predicate::After {
                    key: "due".into(),
                    value: "2026".into(),
                },
                r#"{op: after, key: "due", value: "2026"}"#,
            ),
            (
                Predicate::Matches { query: "q".into() },
                r#"{op: matches, query: "q"}"#,
            ),
            (
                Predicate::Path {
                    glob: "a/**".into(),
                },
                r#"{op: path, glob: "a/**"}"#,
            ),
            (
                Predicate::LinksTo {
                    target: target.clone(),
                },
                r#"{op: links_to, target: "Plan"}"#,
            ),
            (
                Predicate::Resolves { target },
                r#"{op: resolves, target: "Plan"}"#,
            ),
            (
                Predicate::Tag {
                    name: "draft".into(),
                },
                r#"{op: tag, name: "draft"}"#,
            ),
            (
                Predicate::HasFinding {
                    kind: FindingKind::Broken,
                },
                r#"{op: has_finding, kind: "link/broken"}"#,
            ),
        ];
        for (predicate, expected) in spelled {
            assert_eq!(predicate.to_string(), expected);
        }
    }
}
