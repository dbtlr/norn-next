//! What a plan would do, as a preview and a refusal report it.
//!
//! **A forecast is an answer, not a plan.** It is read the way every answer
//! is, dropping a field it does not know.
//!
//! **A forecast carries only what the plan beside it does not.** It always
//! crosses beside a resolved plan, which already names each target and its
//! two states, so a forecast repeating them would carry every transition of
//! a vault-wide preview twice. What the plan cannot say is left: which targets
//! drifted, and the folders the plan makes and removes.
//!
//! **A drifted target is marked, not judged.** A fresh plan resolved after a
//! refusal marks every target that drifted, because a hash cannot tell a file
//! edited after this plan landed on it from one edited before: the mark says
//! the target may already carry this plan's change, and applying the fresh
//! plan is the caller's decision. A preview resolved from what the vault
//! holds has no drifted target.
//!
//! **A folder is its own path type.** Folders are not transitions, and the
//! forecast and the applied report name the folders a plan makes and removes.
//! A document path's grammar would fit a folder, but its published
//! description says it is where a document stands, so a folder path is the
//! same two rules under a name and a description of its own.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::address::IllegalPath;
use crate::document::DocumentPath;

/// What a folder path is called in a refusal that names one.
const FOLDER_PATH: &str = "folder path";

/// Where a folder stands in its vault, relative to the vault root.
///
/// On the wire a folder path is the string itself: `"notes/archive"`. It is
/// not empty and it does not start with a slash.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct FolderPath(String);

impl FolderPath {
    /// The folder path `text` spells, or the reason it spells none.
    pub fn new(text: impl AsRef<str>) -> Result<Self, IllegalPath> {
        let text = text.as_ref();
        if text.is_empty() {
            return Err(IllegalPath::new(
                text,
                FOLDER_PATH,
                "a folder path names something rather than nothing",
            ));
        }
        if text.starts_with('/') {
            return Err(IllegalPath::new(
                text,
                FOLDER_PATH,
                "a folder path is relative to the vault root",
            ));
        }
        Ok(FolderPath(text.to_string()))
    }

    /// The path as the string it is.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FolderPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for FolderPath {
    /// A folder path arrives as the string it is written as and is read
    /// through the grammar, so a path that crossed is a path that parsed.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        FolderPath::new(text).map_err(D::Error::custom)
    }
}

impl JsonSchema for FolderPath {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("FolderPath")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::FolderPath")
    }

    /// A string with a floor of one character, and the rule against a leading
    /// slash stated in the description, as a document path states it.
    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "Where a folder stands in its vault, relative to the vault root. Not empty, and never starting with a slash.",
            "minLength": 1,
        })
    }
}

/// What a resolved plan would do that the plan itself does not say: which of
/// its targets drifted, and the folders it makes and removes.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Forecast {
    /// Each target that held neither its before-state nor its after-state
    /// when the plan was resolved afresh. A drifted target may already carry
    /// this plan's change. Empty on a preview.
    pub drifted: Vec<DocumentPath>,
    /// The folders the plan makes for the files it creates.
    pub folders_made: Vec<FolderPath>,
    /// The folders the plan's removals leave empty, which it removes.
    pub folders_removed: Vec<FolderPath>,
}

impl Forecast {
    /// A plan whose targets `drifted` drifted, making `folders_made` and
    /// removing `folders_removed`.
    pub const fn new(
        drifted: Vec<DocumentPath>,
        folders_made: Vec<FolderPath>,
        folders_removed: Vec<FolderPath>,
    ) -> Self {
        Forecast {
            drifted,
            folders_made,
            folders_removed,
        }
    }
}
