//! The content hash a plan records a file's state by.
//!
//! **One algorithm, named in the string.** A hash is the SHA-256 of a file's
//! exact bytes, the hash the filesystem layer computes over vault bytes, and
//! the wire spells it with its algorithm as a prefix so a hash from any other
//! algorithm — of the same width or not — is refused at the read rather than
//! compared and found unequal, which would read as drift.
//!
//! **The digest's bytes are the constructor, not a dependency.** A caller
//! holding a digest from the filesystem layer builds a hash from its 32 bytes,
//! and [`ContentHash::hex`] hands back the digits that layer parses, so the
//! two spellings meet without this crate linking the layer that hashes.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

/// The prefix naming the algorithm.
const PREFIX: &str = "sha256:";

/// How many hexadecimal digits a SHA-256 digest is spelled in.
const DIGITS: usize = 64;

/// The SHA-256 of a file's exact bytes: `sha256:` and then 64 lowercase
/// hexadecimal digits.
///
/// On the wire a hash is the string itself:
/// `"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"`.
/// A string outside the grammar is refused rather than read as a hash.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ContentHash(String);

impl ContentHash {
    /// The grammar as a regular expression, which is how a schema advertises
    /// it.
    pub const PATTERN: &'static str = "^sha256:[0-9a-f]{64}$";

    /// The hash whose SHA-256 digest is `digest`.
    pub fn from_sha256(digest: [u8; 32]) -> Self {
        let mut text = String::with_capacity(PREFIX.len() + DIGITS);
        text.push_str(PREFIX);
        for byte in digest {
            fmt::Write::write_fmt(&mut text, format_args!("{byte:02x}"))
                .expect("writing into a string");
        }
        ContentHash(text)
    }

    /// The hash `text` spells, or the reason it spells none.
    pub fn new(text: impl AsRef<str>) -> Result<Self, IllegalContentHash> {
        let text = text.as_ref();
        let digits = text.strip_prefix(PREFIX).ok_or(IllegalContentHash)?;
        let lowercase_hex = digits
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if digits.len() != DIGITS || !lowercase_hex {
            return Err(IllegalContentHash);
        }
        Ok(ContentHash(text.to_string()))
    }

    /// The hash as the string it is on the wire.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The 64 lowercase hexadecimal digits of the digest, without the prefix.
    pub fn hex(&self) -> &str {
        &self.0[PREFIX.len()..]
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A string that spells no content hash.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IllegalContentHash;

impl fmt::Display for IllegalContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a content hash is `sha256:` and then 64 lowercase hexadecimal digits")
    }
}

impl std::error::Error for IllegalContentHash {}

impl<'de> Deserialize<'de> for ContentHash {
    /// A hash arrives as the string it is written as and is read through the
    /// grammar, so a hash that crossed is a hash that parsed.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        ContentHash::new(text).map_err(D::Error::custom)
    }
}

impl JsonSchema for ContentHash {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("ContentHash")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::ContentHash")
    }

    /// The pattern the reader keeps, so a surface validating against it
    /// refuses the strings this crate refuses.
    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "The SHA-256 of a file's exact bytes: `sha256:` and then 64 lowercase hexadecimal digits.",
            "pattern": ContentHash::PATTERN,
        })
    }
}
