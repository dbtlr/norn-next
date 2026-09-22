//! How a request addresses one document, and one place inside it.
//!
//! **A target is written as one string and carried as one string.** It is what
//! a person types and what a Markdown link holds, so a shape that split it
//! into fields on the wire would make two spellings of one thing and leave
//! every surface to choose between them. The read path is the constructor: the
//! string is parsed once, here, and what a consumer reads afterwards is the
//! parse.
//!
//! **The address half is a path suffix, and this crate does not re-check it.**
//! Resolution is right-to-left and segment-aligned — `glossary` addresses any
//! `**/glossary.md`, `norn/glossary` only `**/norn/glossary.md` — and which
//! documents a suffix reaches is a question about a vault's contents that the
//! store answers over its index. What the grammar here decides is only what
//! *can* be an address at all: something rather than nothing.
//!
//! **The anchor half is decided by one character.** A `#^` opens a block
//! anchor and a bare `#` opens a heading anchor, which is the Markdown link
//! family's own distinction, spelled once here rather than at each surface
//! that renders a target.
//!
//! **Nothing is percent-decoded.** Percent-decoding belongs to resolving a
//! Markdown link against a vault, where what was encoded is known; a target a
//! client wrote is the literal text it wrote, and decoding it here would make
//! `a%2Fb` and `a/b` one address when a person typed two.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

/// The character that opens an anchor.
const ANCHOR: char = '#';

/// The character that marks an anchor as a block rather than a heading.
const BLOCK: char = '^';

/// Which place inside a document a target names.
///
/// On the wire an anchor is not a value of its own: it is the tail of the
/// target string, after the first `#`. Read back it is an object tagged
/// `kind`: `{"kind":"heading","text":"Design"}`, `{"kind":"block","id":"a1"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Anchor {
    /// A heading in the document, written `#Design`.
    #[non_exhaustive]
    Heading {
        /// The heading text, as written after the `#`.
        text: String,
    },
    /// A block in the document, written `#^a1`.
    #[non_exhaustive]
    Block {
        /// The block identifier, as written after the `#^`.
        id: String,
    },
}

impl Anchor {
    /// A heading anchor for `text`.
    pub fn heading(text: impl Into<String>) -> Self {
        Anchor::Heading { text: text.into() }
    }

    /// A block anchor for `id`.
    pub fn block(id: impl Into<String>) -> Self {
        Anchor::Block { id: id.into() }
    }

    /// The anchor as it is written, `#` included.
    fn written(&self) -> String {
        match self {
            Anchor::Heading { text } => format!("{ANCHOR}{text}"),
            Anchor::Block { id } => format!("{ANCHOR}{BLOCK}{id}"),
        }
    }
}

/// What a request names one document by, and one place inside it.
///
/// On the wire a target is the string it is written as:
/// `"norn/glossary"`, `"glossary#Design"`, `"glossary#^a1"`. The text before
/// the first `#` is a right-to-left, segment-aligned path suffix; the tail
/// after it is a heading anchor, or a block anchor where it opens with `^`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolutionTarget {
    address: String,
    anchor: Option<Anchor>,
}

impl ResolutionTarget {
    /// The target `text` spells, or the reason it spells none.
    pub fn new(text: impl AsRef<str>) -> Result<Self, IllegalTarget> {
        let text = text.as_ref();
        let (address, anchor) = match text.split_once(ANCHOR) {
            None => (text, None),
            Some((address, tail)) => {
                let anchor = match tail.strip_prefix(BLOCK) {
                    Some(id) => Anchor::block(id),
                    None => Anchor::heading(tail),
                };
                (address, Some(anchor))
            }
        };
        if address.is_empty() {
            return Err(IllegalTarget {
                target: text.to_string(),
                problem: "a target names a document by a path suffix, and this one names none",
            });
        }
        Ok(ResolutionTarget {
            address: address.to_string(),
            anchor,
        })
    }

    /// The path suffix the target addresses a document by.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The place inside that document the target names, where it names one.
    pub const fn anchor(&self) -> Option<&Anchor> {
        self.anchor.as_ref()
    }
}

impl fmt::Display for ResolutionTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.address)?;
        match &self.anchor {
            None => Ok(()),
            Some(anchor) => f.write_str(&anchor.written()),
        }
    }
}

impl Serialize for ResolutionTarget {
    /// A target is the string it was written as. The parse is canonical — one
    /// written string has one reading, and one reading has one spelling — so
    /// rendering the parts back is the text that arrived.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ResolutionTarget {
    /// A target arrives as the string it is written as and is read through the
    /// grammar, so a target that crossed is a target that parsed.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        ResolutionTarget::new(text).map_err(D::Error::custom)
    }
}

impl JsonSchema for ResolutionTarget {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("ResolutionTarget")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::ResolutionTarget")
    }

    /// A string with the grammar stated in words. There is no `pattern`: what
    /// an address may hold is a path suffix, and the one rule a regular
    /// expression could carry — that the text before the first `#` is not
    /// empty — is the whole of what the reader checks and is said plainly.
    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "What a request names one document by: a path suffix, optionally followed by `#` and a heading, or `#^` and a block identifier. The path suffix is not empty.",
        })
    }
}

/// A string that spells no resolution target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IllegalTarget {
    target: String,
    problem: &'static str,
}

impl IllegalTarget {
    /// The string that was offered as a target.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// What the grammar wanted instead.
    pub const fn problem(&self) -> &'static str {
        self.problem
    }
}

impl fmt::Display for IllegalTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is not a resolution target: {}",
            self.target, self.problem
        )
    }
}

impl std::error::Error for IllegalTarget {}
