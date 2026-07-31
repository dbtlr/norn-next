//! The name a vault is keyed by, everywhere it is keyed by one.
//!
//! One string does three jobs at once: it is the key of a table in the
//! registry file, it is a directory component under the data directory, and it
//! is the authority-ish identifier a `norn://` address would carry. The
//! grammar is the intersection of what all three accept, not the union.

use std::fmt;

use crate::error::ConfigError;

/// A vault's name: `[a-z][a-z0-9+.-]*`.
///
/// The grammar is RFC 3986's scheme production — a lowercase letter, then
/// lowercase letters, digits, `+`, `.` and `-`. It is deliberately **stricter
/// than `norn-text`'s protocol-identifier grammar**, which admits `_`, and the
/// two are not to be aligned: a protocol identifier is read out of a document
/// and has to accept what documents contain, while a vault name is minted by a
/// person registering a vault and becomes a directory name and a URL
/// component. Widening this to match the reader would put an underscore into
/// paths and addresses that a scheme cannot carry.
///
/// **Parse, don't validate, and no bypass.** There is no unchecked
/// constructor and no force flag: a name outside the grammar has no
/// representation, so no later code has to ask whether the one it holds was
/// checked. The grammar also makes the dangerous directory components
/// unspellable for free — a name cannot be empty, cannot be `.` or `..`, and
/// cannot contain a separator, because it must open with a lowercase letter
/// and holds no `/`.
///
/// The length bound is the other half of "this is a directory component":
/// [`VaultName::MAXIMUM_BYTES`] is `NAME_MAX`, and a name past it is a
/// registry entry whose derived-state directory cannot be created at all.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct VaultName(String);

impl VaultName {
    /// The longest a name may be: `NAME_MAX`, the bound every filesystem norn
    /// runs on puts on one directory component.
    pub const MAXIMUM_BYTES: usize = 255;

    /// The name `text` spells, or the reason it spells none.
    pub fn new(text: impl AsRef<str>) -> Result<Self, ConfigError> {
        let text = text.as_ref();
        let refuse = |problem: &'static str| {
            Err(ConfigError::IllegalName {
                name: text.to_string(),
                problem,
            })
        };

        if text.len() > VaultName::MAXIMUM_BYTES {
            return refuse("a name is at most 255 bytes, which is what a directory component is");
        }
        let mut characters = text.chars();
        let Some(first) = characters.next() else {
            return refuse("a name is at least one character");
        };
        if !first.is_ascii_lowercase() {
            return refuse("a name opens with a lowercase ASCII letter");
        }
        for character in characters {
            let legal = character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '+' | '.' | '-');
            if !legal {
                return refuse(
                    "a name holds only lowercase ASCII letters, digits, `+`, `.` and `-`",
                );
            }
        }
        Ok(VaultName(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for VaultName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for VaultName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
