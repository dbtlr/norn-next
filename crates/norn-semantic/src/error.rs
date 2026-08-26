//! What the engine refuses with, and which refusals say the sidecar is damaged.

use std::fmt;

use norn_db::DbError;
use norn_embed::EmbedError;
use norn_store::StoreError;

/// The engine's one refusal shape.
///
/// Three seams meet here and each keeps its own vocabulary: the sidecar's
/// substrate ([`DbError`]), the lane-1 record the engine derives from
/// ([`StoreError`]), and the embedder ([`EmbedError`]). The engine adds one
/// judgment of its own — [`EngineError::SidecarDamaged`] — because damage is
/// the refusal the owner resolves differently: a damaged sidecar is discarded
/// and rebuilt ([`crate::Engine::discard_and_reopen`]), while every other
/// refusal is an operation to retry or report.
#[derive(Debug)]
pub enum EngineError {
    /// The sidecar's substrate refused, and the refusal is not about the
    /// file's own contents.
    Db(DbError),
    /// The lane-1 store refused a feed page or a document fetch. The store's
    /// own damage stays inside this variant — what to do about a damaged
    /// store is its owner's call, never the engine's.
    Store(StoreError),
    /// The embedder was asked and produced nothing. Carries the document the
    /// input came from, because the model's own refusal does not know it.
    Embed { path: String, error: EmbedError },
    /// The embedder answered at a width other than the one it promises.
    /// Refused at the write, because a narrow row would decode cleanly and
    /// then score against a prefix of every query.
    WrongWidth {
        path: String,
        promised: usize,
        produced: usize,
    },
    /// The sidecar file's own contents are wrong. Discarding and rebuilding
    /// the sidecar is the resolution — derived state is rebuildable by
    /// construction, and the wholesale rebuild is the permanent
    /// always-correct floor (ADR 0021).
    SidecarDamaged { what: String },
}

impl EngineError {
    /// What the engine says is damaged, where this refusal is a statement
    /// about the sidecar's own contents rather than about anything else.
    pub fn sidecar_damage(&self) -> Option<&str> {
        match self {
            EngineError::SidecarDamaged { what } => Some(what),
            EngineError::Db(_)
            | EngineError::Store(_)
            | EngineError::Embed { .. }
            | EngineError::WrongWidth { .. } => None,
        }
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::Db(error) => write!(f, "{error}"),
            EngineError::Store(error) => write!(f, "the lane-1 store refused: {error}"),
            EngineError::Embed { path, error } => {
                write!(f, "embedding `{path}` failed: {error}")
            }
            EngineError::WrongWidth {
                path,
                promised,
                produced,
            } => {
                write!(
                    f,
                    "embedding `{path}` produced {produced} values and the model promises \
                     {promised}"
                )
            }
            EngineError::SidecarDamaged { what } => {
                write!(f, "the sidecar is damaged: {what}")
            }
        }
    }
}

impl std::error::Error for EngineError {}

impl From<DbError> for EngineError {
    /// The substrate's damage judgment is adopted, not re-made: a
    /// [`DbError::Damaged`] is the sidecar's own contents being wrong, and
    /// everything else crosses as the substrate refusal it is.
    fn from(error: DbError) -> Self {
        match error {
            DbError::Damaged { what } => EngineError::SidecarDamaged { what },
            other => EngineError::Db(other),
        }
    }
}

impl From<StoreError> for EngineError {
    fn from(error: StoreError) -> Self {
        EngineError::Store(error)
    }
}

/// Wrap a driver error met on the sidecar connection, splitting damage from
/// everything else the way the substrate does.
pub(crate) fn sql(operation: &'static str, error: norn_db::rusqlite::Error) -> EngineError {
    if norn_db::is_damaged(&error) {
        EngineError::SidecarDamaged {
            what: format!("{operation} failed: {error}"),
        }
    } else {
        EngineError::Db(norn_db::sql(operation, error))
    }
}
