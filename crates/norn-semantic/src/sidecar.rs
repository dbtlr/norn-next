//! Opening the sidecar: what the open ceremony's verdict means for this
//! engine, and the model gate on top of it.
//!
//! The ceremony is [`norn_db::open`] — connect, read the pinned scalars,
//! compare the version, the DDL fingerprint and the schema digest, and either
//! adopt what is there or remove it and create from zero. What this module
//! owns is the sidecar's half of it: the statement list, the model the sidecar
//! records inside the create transaction, and the verdict over that model once
//! the mechanics call the database usable.
//!
//! A sidecar rebuild is always safe: every row is a projection of lane-1
//! records and the feed cursors reset with the epoch, so the next drain
//! recomputes exactly what a fresh sidecar is missing. That is why every
//! verdict short of a broken environment resolves to a rebuild rather than a
//! refusal.

use std::path::Path;

use norn_db::rusqlite::Connection;
use norn_db::{Adoption, OpenOutcome as SidecarOutcome};
use norn_embed::Model;

use crate::ddl;
use crate::error::EngineError;

/// Open, or create, the sidecar at `path` for `model`.
pub(crate) fn open(
    path: &Path,
    model: &Model,
) -> Result<(Connection, SidecarOutcome), EngineError> {
    norn_db::open(path, &sidecar_schema(), &SidecarClient { model })
}

/// Remove the sidecar and create it again from the statement list.
///
/// The route an owner takes deliberately, over damage an open cannot see. The
/// open reaches the same act by itself where the file it met is not one this
/// build wrote.
pub(crate) fn rebuild(path: &Path, model: &Model) -> Result<Connection, EngineError> {
    norn_db::rebuild(path, &sidecar_schema(), &SidecarClient { model })
}

/// The statement list this build writes, and what the ceremony calls it.
fn sidecar_schema() -> norn_db::Schema {
    norn_db::Schema {
        operations: norn_db::schema_operations!("sidecar schema"),
        version: ddl::ENGINE_SCHEMA_VERSION,
        statements: ddl::statements(),
        fingerprint: ddl::fingerprint(),
    }
}

/// What the open ceremony asks this crate, once it has judged the mechanics.
///
/// The sidecar's own pinned keys are the model it embeds with. The ceremony
/// writes neither and reads neither — it hands the create transaction over for
/// them, and hands the database over to be judged by them.
struct SidecarClient<'a> {
    model: &'a Model,
}

impl norn_db::Client for SidecarClient<'_> {
    type Error = EngineError;

    fn record(&self, transaction: &Connection) -> Result<(), EngineError> {
        norn_db::meta::put_meta(transaction, ddl::meta::ENGINE_MODEL_ID, self.model.id())?;
        norn_db::meta::put_meta(
            transaction,
            ddl::meta::ENGINE_MODEL_VERSION,
            self.model.version(),
        )?;
        Ok(())
    }

    /// A shape this build wrote, holding another model's rows and cursors, is
    /// rebuilt from zero.
    ///
    /// The cursors are model-blind — the drained feed is drained for whichever
    /// model was embedding — so adopting them under a new model would skip
    /// everything the old one consumed. A moved model is resolved by the
    /// wholesale-rebuild floor: fresh sidecar, full recompute.
    fn adopt(&self, connection: &Connection, _path: &Path) -> Result<Adoption, EngineError> {
        let recorded_id: Option<String> =
            norn_db::meta::get_meta(connection, ddl::meta::ENGINE_MODEL_ID)?;
        let recorded_version: Option<String> =
            norn_db::meta::get_meta(connection, ddl::meta::ENGINE_MODEL_VERSION)?;
        if recorded_id.as_deref() == Some(self.model.id())
            && recorded_version.as_deref() == Some(self.model.version())
        {
            return Ok(Adoption::Keep);
        }
        Ok(Adoption::Rebuild {
            detail: format!(
                "the sidecar was written for model {}/{} and this engine embeds with {}/{}",
                recorded_id.as_deref().unwrap_or("nothing"),
                recorded_version.as_deref().unwrap_or("nothing"),
                self.model.id(),
                self.model.version(),
            ),
        })
    }
}
