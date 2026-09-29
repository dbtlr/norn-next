//! What a plan would do, as a preview and a refusal report it.
//!
//! **A forecast is an answer, not a plan.** It is read the way every answer
//! is, dropping a field it does not know; the file states inside it are plan
//! types and refuse one wherever they are read.
//!
//! **A drifted target is marked, not judged.** A fresh plan resolved after a
//! refusal marks every target that drifted, because a hash cannot tell a file
//! edited after this plan landed on it from one edited before: the mark says
//! the target may already carry this plan's change, and applying the fresh
//! plan is the caller's decision.
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
use crate::plan::document::FileState;

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

/// One file a plan changes, as a forecast names it.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ForecastTarget {
    /// The file changed.
    pub path: DocumentPath,
    /// What the file must hold before the write.
    pub before: FileState,
    /// What the file holds after it.
    pub after: FileState,
    /// Whether the file held neither state when the plan was resolved afresh.
    /// A drifted file may already carry this plan's change.
    pub drifted: bool,
}

impl ForecastTarget {
    /// The file at `path`, changing from `before` to `after`, not drifted.
    pub const fn new(path: DocumentPath, before: FileState, after: FileState) -> Self {
        ForecastTarget {
            path,
            before,
            after,
            drifted: false,
        }
    }

    /// The same target, marked drifted.
    #[must_use]
    pub const fn drifted(mut self) -> Self {
        self.drifted = true;
        self
    }
}

/// What a resolved plan would do: each file it changes, and the folders it
/// makes and removes.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Forecast {
    /// Each file the plan changes.
    pub targets: Vec<ForecastTarget>,
    /// The folders the plan makes for the files it creates.
    pub folders_made: Vec<FolderPath>,
    /// The folders the plan's removals leave empty, which it removes.
    pub folders_removed: Vec<FolderPath>,
}

impl Forecast {
    /// A plan changing `targets`, making `folders_made` and removing
    /// `folders_removed`.
    pub const fn new(
        targets: Vec<ForecastTarget>,
        folders_made: Vec<FolderPath>,
        folders_removed: Vec<FolderPath>,
    ) -> Self {
        Forecast {
            targets,
            folders_made,
            folders_removed,
        }
    }
}
