//! Where a request finds a vault, and the recorded paths a registration is
//! built out of.
//!
//! **A path here is a grammar, not a string.** Two demands, and they are one
//! grammar because every path a registration records or is built under has
//! both. *Absolute*, because a relative path names a different directory to
//! every process that reads it, and this state is read by a service, a CLI and
//! a shim in three different working directories. *UTF-8*, because a recorded
//! path crosses this seam as a string and is written into a text file at rest:
//! bytes that are not text survive neither trip, and rendering them lossily
//! would report success over a different directory from the one that was
//! asked for.
//!
//! **The wire form holds the text, not the platform path.** The constructor
//! refuses everything that is not UTF-8, so the string *is* the path, and
//! [`VaultRoot::as_path`] is a borrow of it rather than a second
//! representation that could disagree with the first.
//!
//! **The read path is the constructor**, as it is for
//! [`VaultName`](crate::VaultName): a string that is empty, relative or
//! otherwise outside the grammar has no representation on either side of the
//! seam, so nothing downstream asks whether the path it holds was checked. The
//! advertised schema says the string is absolute in words rather than in a
//! `pattern`, because absoluteness is a platform question and no regular
//! expression decides it.
//!
//! **One refusal type for both path grammars.** A root and a schema source are
//! refused for the same two reasons, and the refusal names which of them was
//! offered, so the sentence a person reads is about their argument rather than
//! about a rule.
//!
//! **A vault address is one of two things, and the tag says which.** A name is
//! a registration this installation holds. A root is a vault addressed by
//! where it is, without a registration standing behind it; that is a throwaway
//! attach, and the host holds no lifecycle for one today.

use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::name::VaultName;

/// What a path refused for not being text is told.
const NOT_TEXT: &str =
    "a path here crosses the seam as a string and is written into a text file, so it is UTF-8";

/// What a path refused for being relative is told.
const NOT_ABSOLUTE: &str =
    "a path here is absolute, because it is read by processes whose working directories differ";

/// A string that spells no path this vocabulary records.
///
/// It carries what was offered, which of the two path grammars refused it, and
/// what that grammar wanted, because a person who typed one is the reader of
/// all three.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IllegalPath {
    /// What was offered, as text. A path that was not text at all is carried
    /// as its lossy rendering: the refusal exists to be read, so a reader is
    /// shown the path the way the platform displays it rather than handed
    /// bytes, and the rendering may differ from what was typed exactly where
    /// the bytes are what earned the refusal.
    path: String,
    what: &'static str,
    problem: &'static str,
}

impl IllegalPath {
    /// The path that was offered, as text. A path that is not UTF-8 is carried
    /// as its lossy rendering rather than reproduced: the bytes are what
    /// earned the refusal, and what a person reads is the path as the platform
    /// displays it.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Which path it was meant to be: a vault root, a schema source, or one of
    /// the machine-local bases.
    pub const fn what(&self) -> &'static str {
        self.what
    }

    /// What the grammar wanted instead.
    pub const fn problem(&self) -> &'static str {
        self.problem
    }
}

impl fmt::Display for IllegalPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is not a {}: {}",
            self.path, self.what, self.problem
        )
    }
}

impl std::error::Error for IllegalPath {}

/// `path` as text, if it is a path this vocabulary records: absolute, and
/// UTF-8.
///
/// `what` names which path it is, so the refusal reads as a sentence about the
/// caller's argument.
fn text_of<'a>(path: &'a Path, what: &'static str) -> Result<&'a str, IllegalPath> {
    let Some(text) = path.to_str() else {
        return Err(IllegalPath {
            path: path.display().to_string(),
            what,
            problem: NOT_TEXT,
        });
    };
    if !path.is_absolute() {
        return Err(IllegalPath {
            path: text.to_string(),
            what,
            problem: NOT_ABSOLUTE,
        });
    }
    Ok(text)
}

/// `path`, if it is a path this vocabulary records: absolute, and UTF-8.
///
/// The one grammar every recorded path is held to, exposed for the paths that
/// are checked by it without being wrapped in a type of their own — the
/// machine-local config and data bases.
pub fn absolute_path(path: PathBuf, what: &'static str) -> Result<PathBuf, IllegalPath> {
    text_of(&path, what)?;
    Ok(path)
}

/// A vault's root directory: an absolute UTF-8 path.
///
/// On the wire a root is the string itself: `"/home/person/notes"`. Whether it
/// exists, whether it is a directory, and whether it is the same directory as
/// another root are filesystem questions this type does not ask.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct VaultRoot(String);

impl VaultRoot {
    /// What a refusal calls this path.
    const WHAT: &'static str = "vault root";

    /// The root `path` names, or the reason it names none.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, IllegalPath> {
        let path = path.into();
        Ok(VaultRoot(text_of(&path, VaultRoot::WHAT)?.to_string()))
    }

    /// The root as the platform path it addresses.
    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }

    /// The root as the text it is recorded and carried as.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for VaultRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for VaultRoot {
    /// A root arrives as the string it is written as and is read through the
    /// grammar, so a root that crossed is a root that parsed.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        VaultRoot::new(text).map_err(D::Error::custom)
    }
}

impl JsonSchema for VaultRoot {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("VaultRoot")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::VaultRoot")
    }

    /// A string with the grammar stated in words. There is no `pattern`:
    /// whether a path is absolute is a platform question, and a regular
    /// expression that answered it on one platform would answer it wrongly on
    /// another.
    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "A vault's root directory: an absolute UTF-8 path.",
        })
    }
}

/// Where a vault's schema is read from, when it is not the in-vault default:
/// an absolute UTF-8 path.
///
/// On the wire a source is the string itself. Whether the file exists, and
/// what is in it, belong to whoever resolves the entry.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SchemaSource(String);

impl SchemaSource {
    /// What a refusal calls this path.
    const WHAT: &'static str = "schema source";

    /// The schema source `path` names, or the reason it names none.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, IllegalPath> {
        let path = path.into();
        Ok(SchemaSource(
            text_of(&path, SchemaSource::WHAT)?.to_string(),
        ))
    }

    /// The source as the platform path it addresses.
    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }

    /// The source as the text it is recorded and carried as.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SchemaSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SchemaSource {
    /// A source arrives as the string it is written as and is read through the
    /// grammar, so a source that crossed is a source that parsed.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        SchemaSource::new(text).map_err(D::Error::custom)
    }
}

impl JsonSchema for SchemaSource {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("SchemaSource")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::SchemaSource")
    }

    /// A string with the grammar stated in words, on the same terms as
    /// [`VaultRoot`]'s.
    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "Where a vault's schema is read from: an absolute UTF-8 path.",
        })
    }
}

/// A filesystem-watch backend a registration pins in place of the platform's
/// native one.
///
/// On the wire a backend is the flat string itself: `"poll"`. Absence of the
/// field is the native backend, which is why nothing here spells one: a
/// registration says something about watching only when the native backend
/// does not work for its root.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PollBackend {
    /// Walk the tree on an interval instead of subscribing to the platform's
    /// notification API.
    Poll,
}

impl PollBackend {
    /// Every backend the vocabulary holds, in declaration order.
    pub const ALL: [PollBackend; 1] = [PollBackend::Poll];

    /// The backend as the string it is on the wire and at rest.
    pub const fn as_str(&self) -> &'static str {
        match self {
            PollBackend::Poll => "poll",
        }
    }
}

impl fmt::Display for PollBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A string that spells no watch backend the vocabulary holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnknownPollBackend;

impl fmt::Display for UnknownPollBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the string spells no watch backend")
    }
}

impl std::error::Error for UnknownPollBackend {}

impl TryFrom<&str> for PollBackend {
    type Error = UnknownPollBackend;

    /// The backend a string names, found by walking [`PollBackend::ALL`]
    /// against the strings [`PollBackend::as_str`] hands out: reading a
    /// backend back is the inverse of writing it.
    fn try_from(string: &str) -> Result<Self, UnknownPollBackend> {
        Self::ALL
            .into_iter()
            .find(|backend| backend.as_str() == string)
            .ok_or(UnknownPollBackend)
    }
}

/// How a request addresses the vault it is about.
///
/// On the wire an address is an object tagged `by`:
/// `{"by":"name","name":"notes"}`, `{"by":"root","root":"/home/person/notes"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "by", rename_all = "snake_case")]
#[non_exhaustive]
pub enum VaultAddress {
    /// A name this installation's registry holds.
    #[non_exhaustive]
    Name {
        /// The registered name.
        name: VaultName,
    },
    /// A root directory, with no registration behind it. Addressing a vault
    /// this way asks for a throwaway attach, which the host refuses today,
    /// coded `host/unsupported-attach-mode`.
    #[non_exhaustive]
    Root {
        /// The vault's root directory.
        root: VaultRoot,
    },
}

impl VaultAddress {
    /// A vault addressed by its registered `name`.
    pub const fn name(name: VaultName) -> Self {
        VaultAddress::Name { name }
    }

    /// A vault addressed by its `root`.
    pub const fn root(root: VaultRoot) -> Self {
        VaultAddress::Root { root }
    }
}
