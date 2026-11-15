//! The bearer tokens loopback requests carry.
//!
//! **Storage only.** A token here is a label, some bytes and the moment it was
//! written down. What a token *means* — which request it authorizes, how a
//! presented credential is matched against a stored one, when one is rejected
//! — is the serving side's, and none of it is decided here. This module is why
//! there is one writer of the file rather than two.
//!
//! **No token is generated here.** There is no randomness in this crate: the
//! host decides a token should exist and mints its bytes — at its first start
//! where the file holds none, and on a token request after that — and this
//! API writes them down. The host is the one mint site; a client only reads a
//! token. A generator here would put the quality of a credential inside the
//! crate that stores it, where nobody would look for it.
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
//! # What a read refuses besides
//!
//! Every read of the file — [`read`], and the one inside [`mutate`] — opens it
//! without blocking and without following a link at the name, refuses
//! anything but a regular file by its open descriptor, and refuses a file over
//! 1 MiB as unreadable. A FIFO or a device at the name is then an unreadable
//! file rather than a read that waits for a writer, and a read's cost is
//! bounded whatever was put at the name: the serving side reads this file
//! inline on its I/O thread when it changes. A [`mutate`] whose result would
//! be over 1 MiB is refused before it writes, so this crate never leaves a
//! file its own reader refuses.
//!
//! A stored secret that is empty is refused as corrupt, as [`Tokens::add`]
//! refuses to store one.
//!
//! # What a refusal says
//!
//! A refusal of this file names a label, a key and a type, and never a
//! secret. A file that is not TOML is refused with the line and column the
//! parser stopped at and nothing the parser wrote: its excerpt of the line is a
//! secret's line as often as not, and its message quotes a value it cannot hold,
//! which a secret written unquoted is.
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
//! The secret is written as lowercase hex, through [`norn_wire::hex`], the one
//! codec a presented bearer is decoded by too. A token's bytes are arbitrary
//! and a TOML value is text, so they are encoded rather than embedded; hex is
//! chosen over anything denser because it is decodable by inspection and its
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
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use norn_wire::hex;
use toml::{Table, Value};

use crate::ConfigDirs;
use crate::document::{self, Document};
use crate::error::{ConfigError, corrupt};
use crate::file::{self, Sensitivity, Stored};

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
///
/// Its `Debug` prints the tokens, each with its secret redacted, and nothing
/// of the document they were read from: the document holds every secret as
/// text, and a derived `Debug` would print all of them.
#[derive(Clone)]
pub struct Tokens {
    document: Document,
    /// The sub-table each token was read from, keyed the same way as the
    /// tokens, so a key this build does not model survives a rewrite by it.
    raw: BTreeMap<TokenLabel, Table>,
    tokens: BTreeMap<TokenLabel, Token>,
}

impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tokens")
            .field("tokens", &self.tokens.values().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
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

    /// The token labelled `label`, if there is one: one lookup, whatever the
    /// number of tokens. The host's hello is what asks — it looks the token
    /// that keys its MAC up by the label the dialer sent.
    pub fn get(&self, label: &TokenLabel) -> Option<&Token> {
        self.tokens.get(label)
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

    fn load(
        path: &Path,
        bytes: Option<&[u8]>,
        sensitivity: Sensitivity,
    ) -> Result<Self, ConfigError> {
        let document = Document::read(path, bytes, sensitivity)?;
        let section = document.section(path, TOKENS_KEY)?;

        let mut raw = BTreeMap::new();
        let mut tokens = BTreeMap::new();
        for (key, value) in &section {
            let context = format!("token `{key}`");
            let table = document::entry_table(path, &context, value)?;
            let label = TokenLabel::new(key)?;
            let secret = document::required_string(path, &context, &table, SECRET_KEY)?;
            let secret = hex::decode(&secret).ok_or_else(|| {
                corrupt(
                    path,
                    format!("{context}: `{SECRET_KEY}` is not lowercase hexadecimal"),
                )
            })?;
            // The read direction of `add`'s refusal: an empty credential is
            // matched by a request that presents none.
            if secret.is_empty() {
                return Err(corrupt(
                    path,
                    format!(
                        "{context}: `{SECRET_KEY}` is empty, and an empty secret matches a \
                         request that presents none"
                    ),
                ));
            }
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
                Value::String(hex::encode(&token.secret)),
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
    .capped()
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
/// A mutation whose result would be over 1 MiB is refused with
/// [`Io`](ConfigError::Io) and writes nothing: the read cap holds for this
/// crate's own writes too.
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

/// What is at the token file's name, as the serving side compares it from one
/// request to the next: which file (device and inode), its size, and its
/// modification and change times, each to the nanosecond.
///
/// Taken without following a link, so a link planted at the name is a change
/// of fingerprint — and the read that follows refuses it. Two equal
/// fingerprints are the same file in the same state only once the file's
/// times have [settled](SETTLE_WINDOW): a filesystem stamps a write with a
/// clock that ticks in milliseconds, so two writes inside one tick can leave
/// equal fingerprints behind them.
///
/// Crate-private: the serving side and the dialer reach freshness only through
/// [`WatchedTokens`], which is the one place a fingerprint is compared.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Fingerprint {
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl Fingerprint {
    fn of(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Fingerprint {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.size(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }

    /// The later of the file's modification and change times, in nanoseconds
    /// from the Unix epoch: the last moment the filesystem stamped on it.
    fn latest_nanos(&self) -> i128 {
        let nanos = |(seconds, nanoseconds): (i64, i64)| {
            i128::from(seconds) * 1_000_000_000 + i128::from(nanoseconds)
        };
        nanos(self.modified).max(nanos(self.changed))
    }
}

/// How long a token file's times must stand before the moment a fingerprint
/// is taken for that fingerprint to vouch for the file: 2 s.
///
/// A filesystem stamps a write with a coarse clock — one kernel tick on ext4
/// (a few milliseconds), 1 s on some network and older filesystems, 2 s on FAT
/// — so a second write in the tick of the first can leave every field of the
/// fingerprint as the first left it. A fingerprint taken after the file's
/// latest time is at least one window old is out of that reach: any later
/// write is stamped in a later tick, so it differs. One taken sooner is
/// **racy**, and a holder reads the file again rather than trust it. The value
/// is the coarsest tick of a filesystem in common use, FAT's; it is git's
/// "racy timestamp" rule for its index, applied to the token file.
pub(crate) const SETTLE_WINDOW: Duration = Duration::from_secs(2);

/// A fingerprint and the wall-clock moment it was taken: the clock is read
/// right after the `lstat`, so a write landing between the two is stamped no
/// later than that moment, and inside its window.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Taken {
    fingerprint: Option<Fingerprint>,
    at: SystemTime,
}

impl Taken {
    /// Whether this fingerprint is too young to vouch for the file: the
    /// file's latest time is less than [`SETTLE_WINDOW`] before the moment
    /// it was taken, or after it. Nothing at the name is never racy — a file
    /// arriving there is a change of fingerprint whatever its times.
    fn racy(&self) -> bool {
        let Some(fingerprint) = &self.fingerprint else {
            return false;
        };
        let at = match self.at.duration_since(UNIX_EPOCH) {
            Ok(since) => since.as_nanos() as i128,
            Err(before) => -(before.duration().as_nanos() as i128),
        };
        at - fingerprint.latest_nanos() < SETTLE_WINDOW.as_nanos() as i128
    }
}

/// The token file's fingerprint — `None` when nothing is at its name — and
/// the moment `clock` reads right after it.
///
/// One `lstat` of the name and nothing else: no open, no read, so it costs the
/// same whatever the file holds. A stat that fails for any reason but absence
/// is reported as itself.
fn fingerprint(dirs: &ConfigDirs, clock: &WallClock) -> Result<Taken, ConfigError> {
    let metadata = file::name_metadata(&dirs.tokens_file())?;
    let at = clock.now();
    Ok(Taken {
        fingerprint: metadata.map(|metadata| Fingerprint::of(&metadata)),
        at,
    })
}

/// The wall clock a [`WatchedTokens`] reads the moment of each fingerprint
/// from: the system clock, or the clock a suite states.
#[derive(Clone)]
struct WallClock(Arc<dyn Fn() -> SystemTime + Send + Sync>);

impl WallClock {
    fn now(&self) -> SystemTime {
        (self.0)()
    }
}

impl fmt::Debug for WallClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WallClock")
    }
}

/// The token set a long-lived holder serves from, kept fresh against the file
/// by one fingerprint per check while the file is unchanged.
///
/// **Dormant carrier.** The serving layer's bearer middleware and hello, and
/// the dialer's held-lock wait, hold one of these and check it once per
/// request or poll; neither is built yet, so nothing in the workspace reaches
/// it but its suite.
///
/// It holds the fingerprint it last took and the outcome of the read that
/// followed it: the parsed [`Tokens`], or the refusal the read answered with.
/// A [`check`](Self::check) takes the fingerprint once. Unchanged, with a set
/// held, that is the whole cost. Changed — or not taken at all, since a stat
/// that fails says nothing about the file being the one that was read — or
/// with a refusal held, it takes the fingerprint again, then reads, and holds
/// the second fingerprint with the read's outcome. Because the held fingerprint was taken before the read, an
/// edit landing between the two differs from it, and the next check reads
/// again: an edit is never hidden behind a fingerprint taken after it.
///
/// **A changed file that no longer reads fails closed.** A TOML error, an
/// illegal label, an empty secret, a mode reaching beyond the owner, a file
/// over 1 MiB, or a FIFO, a device or a link at the name each leave
/// [`current`](Self::current)
/// answering the refusal, and a holder must refuse every request while it
/// does. The old set is not kept: it would keep a revoked token working. A
/// fingerprint that keeps failing fails closed the same way, with no read.
///
/// **A held refusal is read again at every check.** What a read refused can
/// lift with the file left exactly as it was — a config directory tightened
/// back to `0700`, a descriptor limit that was reached and is not now — so an
/// unchanged fingerprint is no reason to keep refusing. The holder recovers on
/// the first check whose read answers a set. Failing closed therefore costs
/// two fingerprints and a read per check rather than one fingerprint, which is
/// the price of a holder that cannot stay refused once nothing refuses.
///
/// **Declared limits.** A filesystem whose timestamps are coarser than the
/// interval between two changes can miss the second of two same-size changes
/// within one tick, until the next change: an edit made in place, or a
/// replacement renamed over the file that reuses the inode number of the file
/// it replaced. A replacement with an inode of its own is seen by that, and
/// the change time, which the kernel sets on every write and on any attempt to
/// restore the modification time, catches an in-place, same-size edit and a
/// restored modification time otherwise. And the fingerprint is the file's
/// alone: a mode change on the config *directory* does not change it, so a
/// directory loosened after a read is judged at the next change to the file,
/// not before — and a directory tightened after a refused read is judged at
/// the next check, since a held refusal is read again at every one.
#[derive(Debug)]
pub struct WatchedTokens {
    dirs: ConfigDirs,
    clock: WallClock,
    fingerprint: Result<Taken, ConfigError>,
    outcome: Result<Tokens, ConfigError>,
}

/// What one [`WatchedTokens::check`] cost, and what it changed.
///
/// The serving side counts fingerprints and reads from it, and rebuilds
/// anything it derived from the held set — a lookup index — only when the set
/// was replaced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Check {
    fingerprints: u32,
    read: bool,
    replaced: bool,
}

impl Check {
    /// How many fingerprints the check took: 1 when nothing changed, 2 when
    /// something did or a refusal was held.
    pub const fn fingerprints(self) -> u32 {
        self.fingerprints
    }

    /// Whether the check read the token file.
    pub const fn read(self) -> bool {
        self.read
    }

    /// Whether the held outcome — the set, or the refusal a holder fails
    /// closed on — was replaced: by every check that read, and by one whose
    /// fingerprint failed.
    pub const fn replaced(self) -> bool {
        self.replaced
    }
}

impl WatchedTokens {
    /// The token file as it reads now, with the fingerprint taken just before
    /// the read: one fingerprint and one read.
    ///
    /// A read that refuses is held like any later one: the holder fails
    /// closed from the start.
    pub fn read(dirs: &ConfigDirs) -> Self {
        WatchedTokens::read_with_clock(dirs, SystemTime::now)
    }

    /// [`read`](Self::read), with the moment of each fingerprint read from
    /// `clock` rather than the system clock.
    ///
    /// A suite's seam — this crate's, and the serving side's counter bar: a
    /// file's change time cannot be set back, so a holder is shown a
    /// [settled](Self#a-young-fingerprint-is-read-again) file by a clock set
    /// forward rather than by waiting out the window, and a racy one by a
    /// clock set behind.
    pub fn read_with_clock(
        dirs: &ConfigDirs,
        clock: impl Fn() -> SystemTime + Send + Sync + 'static,
    ) -> Self {
        let clock = WallClock(Arc::new(clock));
        let fingerprint = fingerprint(dirs, &clock);
        let outcome = match &fingerprint {
            Ok(_) => read(dirs),
            Err(error) => Err(error.clone()),
        };
        WatchedTokens {
            dirs: dirs.clone(),
            clock,
            fingerprint,
            outcome,
        }
    }

    /// Bring the held set up to date with the file, as the type's
    /// documentation describes, and say what that cost.
    pub fn check(&mut self) -> Check {
        self.check_where(|| {})
    }

    /// [`check`](Self::check), with `between` run after the second fingerprint
    /// and before the read: the window an edit can land in.
    fn check_where(&mut self, between: impl FnOnce()) -> Check {
        let first = fingerprint(&self.dirs, &self.clock);
        // A held refusal is never an unchanged file: what it refused can lift
        // with the file as it was — a directory tightened, a descriptor freed
        // — so while it is held, every check reads again. Nor is a racy held
        // fingerprint: a same-size edit in the tick of the last one would
        // leave it as it is, so until the file settles, every check reads.
        if self.outcome.is_ok()
            && let (Ok(held), Ok(now)) = (&self.fingerprint, &first)
            && held.fingerprint == now.fingerprint
            && !held.racy()
        {
            return Check {
                fingerprints: 1,
                read: false,
                replaced: false,
            };
        }
        let second = fingerprint(&self.dirs, &self.clock);
        between();
        let (read_it, outcome) = match &second {
            Ok(_) => (true, read(&self.dirs)),
            Err(error) => (false, Err(error.clone())),
        };
        self.fingerprint = second;
        self.outcome = outcome;
        Check {
            fingerprints: 2,
            read: read_it,
            replaced: true,
        }
    }

    /// The set to serve from, or the refusal to fail closed on.
    pub fn current(&self) -> Result<&Tokens, &ConfigError> {
        self.outcome.as_ref()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn label(text: &str) -> TokenLabel {
        TokenLabel::new(text).unwrap_or_else(|error| panic!("`{text}` is a label: {error}"))
    }

    /// A clock an hour ahead of the system's: every file a case writes is
    /// settled under it, so what a case pins is the fingerprint.
    fn an_hour_ahead() -> SystemTime {
        SystemTime::now() + Duration::from_secs(3_600)
    }

    fn system_clock() -> WallClock {
        WallClock(Arc::new(SystemTime::now))
    }

    /// The fingerprint a check holds is the one taken *before* its read, so an
    /// edit landing between the two is read by the next check rather than
    /// hidden behind a fingerprint that already describes it.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the bases a case's state lives under.
    fn an_edit_between_the_second_fingerprint_and_the_read_is_read_by_the_next_check() {
        let scratch = norn_testkit::scratch::Scratch::new("norn-config-fresh-window");
        std::fs::create_dir_all(scratch.join("config")).expect("a config base");
        let dirs = ConfigDirs::new(scratch.join("config"), scratch.join("data"))
            .expect("absolute scratch bases");
        mutate(&dirs, |tokens| tokens.add(&label("laptop"), b"first")).expect("a token");
        let mut watched = WatchedTokens::read_with_clock(&dirs, an_hour_ahead);
        mutate(&dirs, |tokens| tokens.add(&label("service"), b"second")).expect("a token");

        let check = watched.check_where(|| {
            mutate(&dirs, |tokens| Ok(tokens.remove(&label("laptop")))).expect("a revocation");
        });
        assert!(check.read());
        let held: Vec<&str> = watched
            .current()
            .expect("a set")
            .labels()
            .map(TokenLabel::as_str)
            .collect();
        assert_eq!(held, ["service"], "the read saw the edit in the window");

        let next = watched.check();
        assert!(
            next.read(),
            "the held fingerprint predates the edit, so the next check reads: {next:?}"
        );
    }

    /// The token file at `scratch`'s config base, with the config directory
    /// made: where a case arranges the file a fingerprint is taken of.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the bases a case's state lives under.
    fn dirs_in(scratch: &norn_testkit::scratch::Scratch) -> ConfigDirs {
        let dirs = ConfigDirs::new(scratch.join("config"), scratch.join("data"))
            .expect("absolute scratch bases");
        std::fs::create_dir_all(dirs.config_dir()).expect("a config directory");
        std::fs::set_permissions(
            dirs.config_dir(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .expect("an owner-only config directory");
        dirs
    }

    /// A fingerprint names what is at the name, a link included, and nothing
    /// at all is `None`.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: planting a file, then a link, at the token file's name.
    fn a_fingerprint_names_what_is_at_the_name() {
        let scratch = norn_testkit::scratch::Scratch::new("norn-config-fingerprint");
        let dirs = dirs_in(&scratch);
        let path = dirs.tokens_file();
        let of = |dirs: &ConfigDirs| {
            fingerprint(dirs, &system_clock())
                .expect("a fingerprint")
                .fingerprint
        };
        assert_eq!(of(&dirs), None);

        std::fs::write(&path, "version = 1\n").expect("a token file");
        let file = of(&dirs).expect("a file");
        assert_eq!(
            of(&dirs),
            Some(file),
            "an untouched file fingerprints the same"
        );

        std::fs::remove_file(&path).expect("removing the file");
        std::os::unix::fs::symlink(scratch.join("elsewhere"), &path).expect("planting a link");
        let link = of(&dirs).expect("a link is something at the name");
        assert_ne!(link, file);
    }

    /// Write `text` at `path`, owner-only, with no fsync: a write as fast as
    /// the filesystem takes one, so two of them can land inside one tick.
    #[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Harness scaffolding: the writes a hand rename-over makes.
    fn write_private(path: &Path, text: &str) {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .and_then(|mut file| file.write_all(text.as_bytes()))
            .unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
    }

    /// What `path` is, as the operating system reports it rather than as a
    /// fingerprint reads it: its inode, its size, and its modification and
    /// change times.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: judging the file apart from the code under test.
    fn stamp_of(path: &Path) -> (u64, u64, (i64, i64), (i64, i64)) {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(path).expect("the file's metadata");
        (
            metadata.ino(),
            metadata.size(),
            (metadata.mtime(), metadata.mtime_nsec()),
            (metadata.ctime(), metadata.ctime_nsec()),
        )
    }

    /// A replacement renamed over the file, of the same size and stamped
    /// inside the same timestamp tick, differs from it only by its inode —
    /// and is seen by that. The case retries until the filesystem's clock
    /// really did give the two the same times, and asserts that it did, so
    /// what it pins is the inode rather than a tick that happened to turn. On a
    /// filesystem whose clock never lets two writes share a tick it says so and
    /// passes, since there the times alone already tell the files apart.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the rename a hand replacement makes.
    fn a_same_size_replacement_within_one_tick_is_seen_by_its_inode() {
        const ATTEMPTS: usize = 1_000;
        let held_text = "version = 1\n\n[tokens.laptop]\nsecret = \"0a0a\"\ncreated = 1\n";
        let next_text = "version = 1\n\n[tokens.laptop]\nsecret = \"0b0b\"\ncreated = 1\n";
        assert_eq!(held_text.len(), next_text.len());

        for _ in 0..ATTEMPTS {
            let scratch = norn_testkit::scratch::Scratch::new("norn-config-fresh-inode");
            let dirs = dirs_in(&scratch);
            let path = dirs.tokens_file();
            let next = dirs.config_dir().join("tokens.next");
            write_private(&path, held_text);
            write_private(&next, next_text);
            let mut watched = WatchedTokens::read_with_clock(&dirs, an_hour_ahead);
            let held = stamp_of(&path);
            std::fs::rename(&next, &path).expect("renaming the replacement over the file");
            let now = stamp_of(&path);

            if (now.1, now.2, now.3) != (held.1, held.2, held.3) {
                // A tick turned between the writes or at the rename: the
                // times tell the two apart, and the case proves nothing.
                continue;
            }
            assert_ne!(now.0, held.0, "the replacement is another inode");

            let check = watched.check();
            assert!(
                check.read(),
                "a replacement differing only by inode: {check:?}"
            );
            let secret = watched
                .current()
                .expect("a set")
                .get(&label("laptop"))
                .expect("a token")
                .secret()
                .to_vec();
            assert_eq!(secret, [0x0b, 0x0b]);
            return;
        }
        // A filesystem whose clock is fine enough that no two writes ever share
        // a tick cannot stage this case, and there the inode is not the only
        // field that tells the two files apart: the times already do. Saying
        // so is the honest outcome; a failure would report a defect that is
        // not there. Coarse-clocked filesystems (ext4 on Linux, where CI runs)
        // stage it within a few attempts.
        eprintln!(
            "skipped: no attempt in {ATTEMPTS} landed both writes and the rename inside one \
             tick, so this filesystem's times alone tell the two files apart"
        );
    }

    /// The revocation race, made deterministic. Two same-size, in-place edits
    /// inside one filesystem tick, with a check between them, leave the
    /// fingerprint that check took — and the second edit revokes the secret
    /// the first left. The case builds that state directly: the holder holds
    /// the set read before the revoking edit beside the fingerprint the edit
    /// left, taken at a moment within the window of the file's times. The
    /// next check must read the file, and serve the revocation.
    #[test]
    #[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Harness scaffolding: the in-place edit the API never makes.
    fn a_same_size_edit_inside_one_tick_is_read_while_the_held_fingerprint_is_racy() {
        use std::io::{Seek, SeekFrom, Write};
        let scratch = norn_testkit::scratch::Scratch::new("norn-config-fresh-racy-revocation");
        let dirs = dirs_in(&scratch);
        let path = dirs.tokens_file();
        let text = "version = 1\n\n[tokens.laptop]\nsecret = \"0a0a\"\ncreated = 1\n";
        write_private(&path, text);
        let mut watched = WatchedTokens::read(&dirs);
        let held_at = watched.fingerprint.as_ref().expect("a fingerprint").at;

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("opening the token file for writing");
        let offset = text.find("0a0a").expect("the secret") as u64;
        file.seek(SeekFrom::Start(offset)).expect("seeking");
        file.write_all(b"0b0b").expect("revoking `0a0a` in place");
        drop(file);

        // The tick the edits shared: the fingerprint after the revocation is
        // the one the holder took before it.
        let left = fingerprint(&dirs, &system_clock()).expect("a fingerprint");
        watched.fingerprint = Ok(Taken {
            fingerprint: left.fingerprint,
            at: held_at,
        });
        assert!(
            watched.fingerprint.as_ref().expect("a fingerprint").racy(),
            "the file was written within the window of the held moment"
        );

        let check = watched.check();
        assert_eq!(
            (check.fingerprints(), check.read(), check.replaced()),
            (2, true, true),
            "an unchanged racy fingerprint is read again: {check:?}"
        );
        let secret = watched
            .current()
            .expect("a set")
            .get(&label("laptop"))
            .expect("a token")
            .secret()
            .to_vec();
        assert_eq!(secret, [0x0b, 0x0b], "the revoked secret is still served");
    }

    /// The racy rule at its edges: a fingerprint is settled exactly when the
    /// file's later time stands a full window before the moment it was taken.
    /// A time after that moment — a clock stepped back, a time set ahead by
    /// hand — is racy, and so is a modification time later than the change
    /// time. Nothing at the name is never racy.
    #[test]
    fn a_fingerprint_is_racy_until_the_files_times_are_a_window_old() {
        let at = |nanos: u64| UNIX_EPOCH + Duration::from_nanos(nanos);
        let file = |modified: (i64, i64), changed: (i64, i64)| Fingerprint {
            device: 1,
            inode: 1,
            size: 1,
            modified,
            changed,
        };
        let taken = |fingerprint: Fingerprint, moment: SystemTime| Taken {
            fingerprint: Some(fingerprint),
            at: moment,
        };
        let window = SETTLE_WINDOW.as_nanos() as u64;
        let stamped = file((100, 0), (100, 500));
        let latest = 100 * 1_000_000_000 + 500;

        assert!(!taken(stamped, at(latest + window)).racy(), "a window old");
        assert!(
            taken(stamped, at(latest + window - 1)).racy(),
            "a nanosecond short"
        );
        assert!(taken(stamped, at(latest)).racy(), "taken in the same tick");
        assert!(
            taken(stamped, at(50 * 1_000_000_000)).racy(),
            "times after the moment"
        );
        assert!(
            taken(file((200, 0), (100, 0)), at(150 * 1_000_000_000)).racy(),
            "a modification time set ahead of the change time"
        );
        assert!(
            !Taken {
                fingerprint: None,
                at: at(0),
            }
            .racy(),
            "nothing at the name"
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
