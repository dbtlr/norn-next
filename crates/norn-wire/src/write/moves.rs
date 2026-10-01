//! `move`: move one document, or every document a folder holds.
//!
//! **What moves is read from the source.** A vault document is a file whose
//! extension is the document extension, `md` in any ASCII case, so a `from`
//! whose last segment carries it names a document and compiles to
//! `move_document`, and any other `from` names a folder and compiles to
//! `move_folder`. `to` names the same: a document moved to a name that is not
//! a document's, or a folder to one that is, is refused at the read. A folder
//! whose own name carries the extension is not moved by this verb; a plan's
//! `move_folder` names one.
//!
//! **A move carries its link cascade, and nothing turns it off.** Planning
//! rewrites every link naming what is moved, or leaves one as written with
//! the forecast saying why; the request has no flag to leave links alone, to
//! overwrite a destination, or to make parents. A destination must be
//! vacant, and the folders above it are made as a create's are.
//!
//! **The request is read by hand, because `from` and `to` are read
//! together.** Which grammar reads them is decided by `from`, so the request
//! is read by the derive into a private shape holding every key it may carry
//! as written, and the two ends are then read as one [`MoveSubject`]. That
//! shape is also the schema the request advertises.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::address::VaultAddress;
use crate::apply::ApplyMode;
use crate::document::{DOCUMENT_EXTENSION, DocumentPath};
use crate::plan::document::AuthoredPlan;
use crate::plan::forecast::FolderPath;
use crate::plan::operation::{AuthorCondition, Operation, OperationKind};
use crate::write::is_false;

/// What a `move` moves: one document, or every document a folder holds.
///
/// On the wire it is the request's `from` and `to`, each the path as written:
/// `"from":"notes/a.md","to":"archive/a.md"`, `"from":"notes","to":"archive/notes"`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum MoveSubject {
    /// The document at `from`, moved to `to`.
    #[non_exhaustive]
    Document {
        /// Where the document stands.
        from: DocumentPath,
        /// Where it is moved to.
        to: DocumentPath,
    },
    /// Every document the folder `from` holds, moved to the same place under
    /// `to`.
    #[non_exhaustive]
    Folder {
        /// The folder whose documents are moved.
        from: FolderPath,
        /// The folder they are moved to.
        to: FolderPath,
    },
}

impl MoveSubject {
    /// What moving `from` to `to` moves, read from `from`: a document where
    /// its last segment carries the document extension, and a folder
    /// otherwise; or why the two ends name no one move.
    pub fn new(from: impl AsRef<str>, to: impl AsRef<str>) -> Result<Self, IllegalMove> {
        let (from, to) = (from.as_ref(), to.as_ref());
        let refused = |problem: String| IllegalMove {
            from: from.to_string(),
            to: to.to_string(),
            problem,
        };
        match (names_a_document(from), names_a_document(to)) {
            (true, true) => Ok(MoveSubject::Document {
                from: DocumentPath::new(from).map_err(|path| refused(path.to_string()))?,
                to: DocumentPath::new(to).map_err(|path| refused(path.to_string()))?,
            }),
            (false, false) => Ok(MoveSubject::Folder {
                from: FolderPath::new(from).map_err(|path| refused(path.to_string()))?,
                to: FolderPath::new(to).map_err(|path| refused(path.to_string()))?,
            }),
            (true, false) => Err(refused(format!(
                "`{from}` names a document, and `{to}` does not carry the document extension `.{DOCUMENT_EXTENSION}`"
            ))),
            (false, true) => Err(refused(format!(
                "`{from}` names a folder, and `{to}` names a document"
            ))),
        }
    }

    /// The document move at `from`, to `to`.
    pub const fn document(from: DocumentPath, to: DocumentPath) -> Self {
        MoveSubject::Document { from, to }
    }

    /// The folder move from `from` to `to`.
    pub const fn folder(from: FolderPath, to: FolderPath) -> Self {
        MoveSubject::Folder { from, to }
    }

    /// The operation kind this move is.
    fn into_kind(self) -> OperationKind {
        match self {
            MoveSubject::Document { from, to } => OperationKind::MoveDocument { from, to },
            MoveSubject::Folder { from, to } => OperationKind::MoveFolder { from, to },
        }
    }
}

/// Whether `path` names a document: its last segment carries the document
/// extension, in any ASCII case, after a dot that does not lead the segment —
/// the rule the vault reads its documents by.
fn names_a_document(path: &str) -> bool {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    match leaf.rfind('.') {
        Some(dot) if dot > 0 => leaf[dot + 1..].eq_ignore_ascii_case(DOCUMENT_EXTENSION),
        _ => false,
    }
}

/// Two ends that name no one move: a path either grammar refuses, or a
/// document and a folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IllegalMove {
    from: String,
    to: String,
    problem: String,
}

impl IllegalMove {
    /// The `from` that was offered.
    pub fn from(&self) -> &str {
        &self.from
    }

    /// The `to` that was offered.
    pub fn to(&self) -> &str {
        &self.to
    }

    /// What is wrong with the two, in words.
    pub fn problem(&self) -> &str {
        &self.problem
    }
}

impl fmt::Display for IllegalMove {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "moving `{}` to `{}` names no move: {}",
            self.from, self.to, self.problem
        )
    }
}

impl std::error::Error for IllegalMove {}

/// What a `move` request carries.
///
/// On the wire the moved paths are `from` and `to` beside the request's other
/// keys: `{"vault":…,"mode":"preview","from":"notes/a.md","to":"archive/a.md"}`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub struct MoveParams {
    /// The vault written.
    pub vault: VaultAddress,
    /// Whether to preview the write or apply it. There is no default.
    pub mode: ApplyMode,
    /// What is moved, and where to.
    #[serde(flatten)]
    pub subject: MoveSubject,
    /// What the author observed and requires to hold.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<AuthorCondition>,
    /// Whether the write applies past the schema check. Absent is `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub force: bool,
}

impl MoveParams {
    /// A request to `mode` the move `subject` names in `vault`, with no
    /// condition and not forced.
    pub const fn new(vault: VaultAddress, mode: ApplyMode, subject: MoveSubject) -> Self {
        MoveParams {
            vault,
            mode,
            subject,
            conditions: Vec::new(),
            force: false,
        }
    }

    /// The request requiring `conditions` to hold.
    #[must_use]
    pub fn with_conditions(mut self, conditions: Vec<AuthorCondition>) -> Self {
        self.conditions = conditions;
        self
    }

    /// The request applying past the schema check where `force` holds.
    #[must_use]
    pub const fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }

    /// The plan the request compiles to: one `move_document` or one
    /// `move_folder`, carrying the request's conditions, forced as the
    /// request is.
    pub fn plan(self) -> AuthoredPlan {
        let MoveParams {
            vault,
            mode: _,
            subject,
            conditions,
            force,
        } = self;
        let operation = Operation::new(subject.into_kind()).with_conditions(conditions);
        AuthoredPlan::new(vault, vec![operation]).with_force(force)
    }
}

/// What a `move` request carries: the document or folder at `from`, moved to
/// `to`.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "MoveParams")]
struct MoveKeys {
    /// The vault written.
    vault: VaultAddress,
    /// Whether to preview the write or apply it. There is no default.
    mode: ApplyMode,
    /// Where the document or folder stands, relative to the vault root. A
    /// path whose last segment carries the document extension `.md`, in any
    /// case, names a document; any other names a folder.
    #[schemars(length(min = 1))]
    from: String,
    /// Where it is moved to: a path carrying the document extension for a
    /// document, and one not carrying it for a folder. Nothing may stand
    /// there.
    #[schemars(length(min = 1))]
    to: String,
    /// What the author observed and requires to hold.
    #[serde(default)]
    conditions: Vec<AuthorCondition>,
    /// Whether the write applies past the schema check. Absent is `false`.
    #[serde(default)]
    force: bool,
}

impl<'de> Deserialize<'de> for MoveParams {
    /// Every key is read by the derive, refusing any the request does not
    /// name and any written twice; the two ends are then read as one move.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let MoveKeys {
            vault,
            mode,
            from,
            to,
            conditions,
            force,
        } = MoveKeys::deserialize(deserializer)?;
        let subject = MoveSubject::new(from, to).map_err(D::Error::custom)?;
        Ok(MoveParams {
            vault,
            mode,
            subject,
            conditions,
            force,
        })
    }
}

impl JsonSchema for MoveParams {
    fn schema_name() -> Cow<'static, str> {
        MoveKeys::schema_name()
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::MoveParams")
    }

    /// The shape the request is read through, which carries `from` and `to`
    /// as the paths they are written as.
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        MoveKeys::json_schema(generator)
    }
}
