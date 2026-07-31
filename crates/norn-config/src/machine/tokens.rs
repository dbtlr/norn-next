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
//! the token is world-readable. The directory it sits in is created at `0700`
//! and checked at every read and write, because a directory the group can
//! traverse is a token file the group can open whatever the file's own mode
//! says. In the other direction, a read of a file — or of a directory — whose
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
//!
//! # What a rewrite preserves
//!
//! A key this build does not model — a scope, an expiry a newer build wrote —
//! survives a rewrite, because the document is held as it was parsed.
//! Comments and key order do not: the file is re-rendered from that document.
//! Retention is also scoped to one token's lifetime rather than to its label,
//! so a label removed and used again is a new token carrying nothing of the
//! old one.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use toml::{Table, Value};

use crate::ConfigDirs;
use crate::document::{self, Document};
use crate::error::{ConfigError, corrupt};
use crate::file::{Sensitivity, Stored};

/// The key of the table holding the tokens.
const TOKENS_KEY: &str = "tokens";

const SECRET_KEY: &str = "secret";
const CREATED_KEY: &str = "created";

/// The name a token is addressed by: `[a-z0-9][a-z0-9._-]*`, at most 64 bytes.
///
/// A label keys a TOML table and is typed by a person at a command line, and
/// the grammar is what both of those can carry without ambiguity: no
/// whitespace, no quoting, no case that reads as two labels depending on who
/// compares them. It is looser than a vault name — a label names a machine or
/// a service rather than a directory and a URL component, so digits may open
/// one and `_` is allowed inside — and it is bounded, because an unbounded
/// label is a table key nothing can display.
///
/// **Parse, don't validate.** There is no unchecked constructor: the grammar
/// holds at the API and at the file, so a hand-edited label outside it is
/// refused on the way in rather than becoming a token nobody can name.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TokenLabel(String);

impl TokenLabel {
    /// The longest a label may be.
    pub const MAXIMUM_BYTES: usize = 64;

    /// The label `text` spells, or the reason it spells none.
    pub fn new(text: impl AsRef<str>) -> Result<Self, ConfigError> {
        let text = text.as_ref();
        let refuse = |problem: &'static str| {
            Err(ConfigError::IllegalLabel {
                label: text.to_string(),
                problem,
            })
        };

        if text.len() > TokenLabel::MAXIMUM_BYTES {
            return refuse("a label is at most 64 bytes");
        }
        let mut characters = text.chars();
        let Some(first) = characters.next() else {
            return refuse("a label is at least one character");
        };
        if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
            return refuse("a label opens with a lowercase ASCII letter or a digit");
        }
        for character in characters {
            let legal = character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '_' | '-');
            if !legal {
                return refuse(
                    "a label holds only lowercase ASCII letters, digits, `.`, `_` and `-`",
                );
            }
        }
        Ok(TokenLabel(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TokenLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for TokenLabel {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// One stored token.
///
/// Its `Debug` does not print the secret. A token reaches a log the moment
/// anything holding one is formatted, and the default derive would put it
/// there.
#[derive(Clone, Eq, PartialEq)]
pub struct Token {
    label: TokenLabel,
    secret: Vec<u8>,
    created: u64,
}

impl Token {
    /// The name this token is addressed by — for removal, and for a person
    /// reading a list of them.
    pub fn label(&self) -> &TokenLabel {
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
    /// The sub-table each token was read from, keyed the same way as the
    /// tokens, so a key this build does not model survives a rewrite by it.
    raw: BTreeMap<TokenLabel, Table>,
    tokens: BTreeMap<TokenLabel, Token>,
}

impl Tokens {
    /// Write down a token under `label`.
    ///
    /// Refuses a label already in use. A label is how a token is addressed for
    /// removal, so a second entry under one label would be a token nobody
    /// could name — replacing the first silently would be worse still, because
    /// the credential that stopped working would have done so without anybody
    /// asking for it.
    ///
    /// Refuses an empty secret, too: an empty credential is matched by a
    /// request that presents nothing at all.
    pub fn add(&mut self, label: &TokenLabel, secret: &[u8]) -> Result<(), ConfigError> {
        if self.tokens.contains_key(label) {
            return Err(ConfigError::DuplicateLabel {
                label: label.to_string(),
            });
        }
        if secret.is_empty() {
            return Err(ConfigError::EmptySecret {
                label: label.to_string(),
            });
        }
        let created = stamp(SystemTime::now())?;
        self.tokens.insert(
            label.clone(),
            Token {
                label: label.clone(),
                secret: secret.to_vec(),
                created,
            },
        );
        Ok(())
    }

    /// Remove the token labelled `label`, saying whether there was one.
    ///
    /// The keys this build does not model go with it: retention is scoped to a
    /// token's lifetime, not to its label, so a rotation through one label
    /// cannot graft a removed token's fields onto the new secret.
    pub fn remove(&mut self, label: &TokenLabel) -> bool {
        self.raw.remove(label);
        self.tokens.remove(label).is_some()
    }

    /// Every label, in order.
    pub fn labels(&self) -> impl Iterator<Item = &TokenLabel> {
        self.tokens.keys()
    }

    /// Every token, in label order — what the serving side verifies a
    /// presented credential against.
    pub fn all(&self) -> impl Iterator<Item = &Token> {
        self.tokens.values()
    }

    fn load(path: &Path, bytes: Option<&[u8]>) -> Result<Self, ConfigError> {
        let document = Document::read(path, bytes)?;
        let section = document.section(path, TOKENS_KEY)?;

        let mut raw = BTreeMap::new();
        let mut tokens = BTreeMap::new();
        for (key, value) in &section {
            let context = format!("token `{key}`");
            let table = document::entry_table(path, &context, value)?;
            let label = TokenLabel::new(key)?;
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
                    label,
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

    /// The token file as TOML, with every key this build does not model still
    /// in the place it was read from.
    ///
    /// Takes the tokens by mutable reference so that the section lands in the
    /// document rather than in a copy of it.
    fn render(&mut self, path: &Path) -> Result<String, ConfigError> {
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
                    corrupt(
                        path,
                        format!("token `{label}`: `{CREATED_KEY}` does not fit"),
                    )
                })?),
            );
            section.insert(label.as_str().to_string(), Value::Table(table));
        }
        self.document.set_section(TOKENS_KEY, section);
        self.document.render(path)
    }
}

/// The token file, as this crate reads and replaces it.
fn stored(dirs: &ConfigDirs) -> Stored<Tokens> {
    Stored::new(
        dirs.tokens_file(),
        Sensitivity::Private,
        Tokens::load,
        Tokens::render,
    )
}

/// Read the token file. Never writes, whatever it finds.
///
/// A file that is not there reads as no tokens. A file the group or the world
/// can read — or one in a directory they can reach into — is refused rather
/// than read.
pub fn read(dirs: &ConfigDirs) -> Result<Tokens, ConfigError> {
    stored(dirs).read()
}

/// Read the token file, hand it to `apply`, and write back what `apply`
/// leaves.
///
/// Held under one exclusive lock from before the read to after the replacement,
/// on the same terms as [`crate::registry::mutate`]: two processes adding a
/// token at once produce two tokens, never one. A refusal from `apply` — a
/// duplicate label, most of all — writes nothing, and neither does a mutation
/// that leaves the file's bytes as they were.
///
/// **`apply` must not call [`mutate`] again.** A nested mutation would wait on
/// its own caller's lock forever, so it is refused with
/// [`NestedMutation`](ConfigError::NestedMutation).
///
/// A [`MutationUnconfirmed`](ConfigError::MutationUnconfirmed) refusal means
/// the token is in the file and its durability is not confirmed. The token was
/// written down: a caller that minted one must treat it as stored — adding it
/// again refuses as a duplicate — rather than minting a second.
pub fn mutate<T>(
    dirs: &ConfigDirs,
    apply: impl FnOnce(&mut Tokens) -> Result<T, ConfigError>,
) -> Result<T, ConfigError> {
    stored(dirs).mutate(apply)
}

/// The seconds since the Unix epoch at `moment`, or a refusal.
///
/// A clock reading before the epoch has no stamp to give, and writing one
/// anyway — the zero a saturating conversion produces — would record 1970 as
/// though it were a fact. The read direction already refuses a negative stamp,
/// and this is the write direction agreeing with it.
fn stamp(moment: SystemTime) -> Result<u64, ConfigError> {
    moment
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .map_err(|_| ConfigError::ClockBeforeEpoch)
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from_digit(u32::from(byte >> 4), 16).expect("a nibble is a hex digit"));
        text.push(char::from_digit(u32::from(byte & 0x0f), 16).expect("a nibble is a hex digit"));
    }
    text
}

/// The bytes `text` spells, or `None` when it spells none.
///
/// Read as bytes rather than as characters: the length, the pairing and the
/// digit lookup are then all in the same unit, and a multi-byte character
/// cannot make a string of `n` digits decode as anything but `n / 2` bytes.
fn decode_hex(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    bytes
        .chunks_exact(2)
        .map(|pair| Some((digit(pair[0])? << 4) | digit(pair[1])?))
        .collect()
}

/// The value of one lowercase hexadecimal digit.
fn digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(text: &str) -> TokenLabel {
        TokenLabel::new(text).unwrap_or_else(|error| panic!("`{text}` is a label: {error}"))
    }

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
        assert_eq!(
            decode_hex("é"),
            None,
            "two bytes that are one character, and neither of them a digit"
        );
    }

    /// A secret in a log is a secret leaked, and the derived `Debug` is how it
    /// would get there.
    #[test]
    fn a_tokens_debug_does_not_carry_its_secret() {
        let token = Token {
            label: label("laptop"),
            secret: b"the-secret-bytes".to_vec(),
            created: 0,
        };
        let printed = format!("{token:?}");
        assert!(!printed.contains("secret-bytes"), "{printed}");
        assert!(printed.contains("laptop"), "{printed}");
    }

    /// The write direction agrees with the read direction: a stamp before the
    /// epoch is refused in both, rather than fabricated as 1970 by one and
    /// rejected by the other.
    #[test]
    fn a_clock_before_the_epoch_has_no_stamp() {
        assert_eq!(stamp(UNIX_EPOCH).expect("the epoch itself"), 0);
        assert_eq!(
            stamp(UNIX_EPOCH + Duration::from_secs(90)).expect("a moment after it"),
            90
        );
        assert_eq!(
            stamp(UNIX_EPOCH - Duration::from_secs(1)).expect_err("a moment before it"),
            ConfigError::ClockBeforeEpoch
        );
    }
}
