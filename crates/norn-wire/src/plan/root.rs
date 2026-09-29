//! The identity of the directory a vault's root is.
//!
//! **One encoding, owned here, and never read back into parts.** The host
//! builds an identity from the root directory's device and inode, and every
//! other holder compares two identities for equality and does nothing else
//! with one. The string is opaque on purpose: a caller that parsed it would
//! pin an encoding this crate is free to change, and the parts name nothing a
//! caller acts on. So there is a constructor from the pair, a reader that
//! accepts exactly the strings that constructor builds, and no accessor for
//! the parts.
//!
//! **What the pair buys and what it costs.** Two registrations over one root,
//! or two installations reading one root, read one identity, so a plan
//! previewed through one applies through the other. A filesystem that
//! renumbers its device when it is mounted again gives the same directory a
//! new identity, so a plan made before the remount refuses: the safe
//! direction, answered by previewing again.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

/// How many hexadecimal digits each half of the pair is spelled in.
const HALF: usize = 16;

/// The strings the constructor builds, as a schema advertises them.
const PATTERN: &str = "^[0-9a-f]{32}$";

/// The identity of the directory a vault's root is: an opaque string, compared
/// only for equality and never read for parts.
///
/// On the wire an identity is the string itself. Two registrations or two
/// installations over one root read one identity. A filesystem that renumbers
/// its device when it is mounted again gives the root a new identity, so a
/// plan made before the remount refuses and is previewed again.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct RootIdentity(String);

impl RootIdentity {
    /// The identity of the directory standing at `inode` on `device`.
    pub fn from_device_and_inode(device: u64, inode: u64) -> Self {
        RootIdentity(format!("{device:016x}{inode:016x}"))
    }

    /// The identity `text` spells, where `text` is a string the constructor
    /// can build: 32 lowercase hexadecimal digits, since every such string is
    /// one device and inode pair.
    fn parse(text: &str) -> Result<Self, IllegalRootIdentity> {
        let lowercase_hex = text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if text.len() != 2 * HALF || !lowercase_hex {
            return Err(IllegalRootIdentity);
        }
        Ok(RootIdentity(text.to_string()))
    }
}

/// A string no root identity is spelled as.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct IllegalRootIdentity;

impl fmt::Display for IllegalRootIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the string is no root identity this vocabulary builds")
    }
}

impl std::error::Error for IllegalRootIdentity {}

impl<'de> Deserialize<'de> for RootIdentity {
    /// An identity arrives as the string it is written as and is read only
    /// where it is a string the constructor builds, so an identity nobody
    /// built is refused rather than compared.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        RootIdentity::parse(&text).map_err(D::Error::custom)
    }
}

impl JsonSchema for RootIdentity {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("RootIdentity")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::RootIdentity")
    }

    /// The pattern of the strings the constructor builds, which is what the
    /// reader accepts.
    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "The identity of the directory a vault's root is: an opaque string, compared only for equality and never read for parts. Two registrations or two installations over one root read one identity. A filesystem that renumbers its device when it is mounted again gives the root a new identity, so a plan made before the remount refuses and is previewed again.",
            "pattern": PATTERN,
        })
    }
}
