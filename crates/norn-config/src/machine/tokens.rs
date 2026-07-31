//! The bearer tokens loopback requests carry.
//!
//! **Storage only.** A token here is a label, some bytes and the moment it was
//! written down. What a token *means* — which request it authorizes, how a
//! presented credential is matched against a stored one, when one is rejected
//! — is the serving side's, and none of it is decided here. This module is why
//! there is one writer of the file rather than two.
//!
//! **No token is generated here.** There is no randomness in this crate: the
//! client decides a token should exist and mints its bytes, and this API
//! writes them down. A generator here would put the quality of a credential
//! inside the crate that stores it, where nobody would look for it.
//!
//! # Permissions
//!
//! The file holds secrets, so it is created at `0600` **before any byte lands
//! in it** — the temporary file the atomic write builds is created with that
//! mode rather than adjusted to it afterwards, so there is no window in which
//! the token is world-readable. In the other direction, a read of a file whose
//! mode reaches beyond its owner is [refused], not repaired: the bytes have
//! been exposed for as long as the mode has stood, and quietly tightening it
//! would hide that from the person who needs to rotate them.
//!
//! [refused]: crate::ConfigError::InsecurePermissions
//!
//! # The file
//!
//! ```toml
//! version = 1
//!
//! [tokens.laptop]
//! secret = "3f9a1c..."
//! created = 1753900000
//! ```
//!
//! The secret is written as lowercase hex. A token's bytes are arbitrary and a
//! TOML value is text, so they are encoded rather than embedded; hex is chosen
//! over anything denser because it is decodable by inspection and its
//! implementation is short enough to be read at a glance.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use toml::{Table, Value};

use crate::document::{self, Document};
use crate::error::{ConfigError, corrupt};
use crate::file;
use crate::{ConfigDirs, error};

/// The key of the table holding the tokens.
const TOKENS_KEY: &str = "tokens";

const SECRET_KEY: &str = "secret";
const CREATED_KEY: &str = "created";

/// One stored token.
///
/// Its `Debug` does not print the secret. A token reaches a log the moment
/// anything holding one is formatted, and the default derive would put it
/// there.
#[derive(Clone, Eq, PartialEq)]
pub struct Token {
    label: String,
    secret: Vec<u8>,
    created: u64,
}

impl Token {
    /// The name this token is addressed by — for removal, and for a person
    /// reading a list of them.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The bytes a presented credential is verified against.
    pub fn secret(&self) -> &[u8] {
        &self.secret
    }

    /// When the token was written down.
    pub fn created(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(self.created)
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Token")
            .field("label", &self.label)
            .field("secret", &"<redacted>")
            .field("created", &self.created)
            .finish()
    }
}

/// The token file, as read.
#[derive(Clone, Debug)]
pub struct Tokens {
    document: Document,
    /// The sub-table each token was read from, so a key this build does not
    /// model survives a rewrite by it.
    raw: BTreeMap<String, Table>,
    tokens: BTreeMap<String, Token>,
}

impl Tokens {
    /// Write down a token under `label`.
    ///
    /// Refuses a label already in use. A label is how a token is addressed for
    /// removal, so a second entry under one label would be a token nobody
    /// could name — replacing the first silently would be worse still, because
    /// the credential that stopped working would have done so without anybody
    /// asking for it.
    pub fn add(&mut self, label: &str, secret: &[u8]) -> Result<(), ConfigError> {
        if self.tokens.contains_key(label) {
            return Err(ConfigError::DuplicateLabel {
                label: label.to_string(),
            });
        }
        let created = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.tokens.insert(
            label.to_string(),
            Token {
                label: label.to_string(),
                secret: secret.to_vec(),
                created,
            },
        );
        Ok(())
    }

    /// Remove the token labelled `label`, saying whether there was one.
    pub fn remove(&mut self, label: &str) -> bool {
        self.tokens.remove(label).is_some()
    }

    /// Every label, in order.
    pub fn labels(&self) -> impl Iterator<Item = &str> {
        self.tokens.keys().map(String::as_str)
    }

    /// The token labelled `label`.
    pub fn get(&self, label: &str) -> Option<&Token> {
        self.tokens.get(label)
    }

    /// Every token, in label order — what the serving side verifies a
    /// presented credential against.
    pub fn all(&self) -> impl Iterator<Item = &Token> {
        self.tokens.values()
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    fn load(path: &Path, bytes: Option<Vec<u8>>) -> Result<Self, ConfigError> {
        let document = Document::read(path, bytes)?;
        let section = document.section(path, TOKENS_KEY)?;

        let mut raw = BTreeMap::new();
        let mut tokens = BTreeMap::new();
        for (label, value) in &section {
            let context = format!("token `{label}`");
            let table = document::entry_table(path, &context, value)?;
            let secret = document::required_string(path, &context, &table, SECRET_KEY)?;
            let secret = decode_hex(&secret).ok_or_else(|| {
                corrupt(
                    path,
                    format!("{context}: `{SECRET_KEY}` is not lowercase hexadecimal"),
                )
            })?;
            let created = document::required_integer(path, &context, &table, CREATED_KEY)?;
            let created = u64::try_from(created).map_err(|_| {
                corrupt(
                    path,
                    format!("{context}: `{CREATED_KEY}` is before the Unix epoch"),
                )
            })?;

            raw.insert(label.clone(), table);
            tokens.insert(
                label.clone(),
                Token {
                    label: label.clone(),
                    secret,
                    created,
                },
            );
        }
        Ok(Tokens {
            document,
            raw,
            tokens,
        })
    }

    fn render(&self, path: &Path) -> Result<String, ConfigError> {
        let mut section = Table::new();
        for (label, token) in &self.tokens {
            let mut table = self.raw.get(label).cloned().unwrap_or_default();
            table.insert(
                SECRET_KEY.to_string(),
                Value::String(encode_hex(&token.secret)),
            );
            table.insert(
                CREATED_KEY.to_string(),
                Value::Integer(i64::try_from(token.created).map_err(|_| {
                    error::corrupt(
                        path,
                        format!("token `{label}`: `{CREATED_KEY}` does not fit"),
                    )
                })?),
            );
            section.insert(label.clone(), Value::Table(table));
        }
        let mut document = self.document.clone();
        document.set_section(TOKENS_KEY, section);
        document.render(path)
    }
}

/// Read the token file. Never writes, whatever it finds.
///
/// A file that is not there reads as no tokens. A file the group or the world
/// can read is refused rather than read.
pub fn read(dirs: &ConfigDirs) -> Result<Tokens, ConfigError> {
    let path = dirs.tokens_file();
    Tokens::load(&path, file::read_private(&path)?)
}

/// Read the token file, hand it to `apply`, and write back what `apply`
/// leaves.
///
/// Held under one exclusive lock from before the read to after the replacement,
/// on the same terms as [`crate::registry::mutate`]: two processes adding a
/// token at once produce two tokens, never one. A refusal from `apply` — a
/// duplicate label, most of all — writes nothing.
pub fn mutate<T>(
    dirs: &ConfigDirs,
    apply: impl FnOnce(&mut Tokens) -> Result<T, ConfigError>,
) -> Result<T, ConfigError> {
    let path = dirs.tokens_file();
    file::locked_update(&path, file::PRIVATE_MODE, Tokens::load, apply, |tokens| {
        tokens.render(&path)
    })
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from_digit(u32::from(byte >> 4), 16).expect("a nibble is a hex digit"));
        text.push(char::from_digit(u32::from(byte & 0x0f), 16).expect("a nibble is a hex digit"));
    }
    text
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let digits: Option<Vec<u8>> = text
        .chars()
        .map(|character| {
            if character.is_ascii_uppercase() {
                return None;
            }
            character.to_digit(16).map(|digit| digit as u8)
        })
        .collect();
    let digits = digits?;
    Some(
        digits
            .chunks(2)
            .map(|pair| (pair[0] << 4) | pair[1])
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_every_byte() {
        let bytes: Vec<u8> = (0..=255u8).collect();
        let text = encode_hex(&bytes);
        assert_eq!(text.len(), 512);
        assert_eq!(decode_hex(&text).expect("a decoding"), bytes);
    }

    #[test]
    fn hex_that_is_not_hex_decodes_to_nothing() {
        assert_eq!(decode_hex("abc"), None, "an odd number of digits");
        assert_eq!(decode_hex("zz"), None, "a digit outside the alphabet");
        assert_eq!(
            decode_hex("AB"),
            None,
            "uppercase, which is not what is written"
        );
    }

    /// A secret in a log is a secret leaked, and the derived `Debug` is how it
    /// would get there.
    #[test]
    fn a_tokens_debug_does_not_carry_its_secret() {
        let token = Token {
            label: "laptop".to_string(),
            secret: b"the-secret-bytes".to_vec(),
            created: 0,
        };
        let printed = format!("{token:?}");
        assert!(!printed.contains("secret-bytes"), "{printed}");
        assert!(printed.contains("laptop"), "{printed}");
    }
}
