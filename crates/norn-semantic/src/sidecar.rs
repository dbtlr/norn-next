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
    ///
    /// A model key that will not read at all reaches the same floor. The value
    /// is part of the shape, so a key holding something no reader of this build
    /// wrote says the file came from somewhere else — and the only reading that
    /// leaves the sidecar in place is one the environment refused.
    fn adopt(&self, connection: &Connection, _path: &Path) -> Result<Adoption, EngineError> {
        let recorded_id = match read_model_key(connection, ddl::meta::ENGINE_MODEL_ID) {
            Reading::Value(value) => value,
            Reading::Damaged(detail) => return Ok(Adoption::Rebuild { detail }),
            Reading::Refused(error) => return Err(error),
        };
        let recorded_version = match read_model_key(connection, ddl::meta::ENGINE_MODEL_VERSION) {
            Reading::Value(value) => value,
            Reading::Damaged(detail) => return Ok(Adoption::Rebuild { detail }),
            Reading::Refused(error) => return Err(error),
        };
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

/// What reading one of the sidecar's own pinned keys produced.
enum Reading {
    /// The value the key holds, or `None` where the key is not set.
    Value(Option<String>),
    /// The key holds something no reader of this build wrote, named as the
    /// detail a rebuild carries.
    Damaged(String),
    /// The environment refused the read.
    Refused(EngineError),
}

/// Read one of the sidecar's own pinned keys under the same damage policy the
/// mechanics verdict is taken under.
///
/// A value the column cannot produce is part of the shape, so it says the file
/// was written by something else and resolves as a rebuild. A busy database, a
/// revoked permission or an I/O error says nothing about the shape and is
/// reported as the refused read it was.
fn read_model_key(connection: &Connection, key: &str) -> Reading {
    match norn_db::meta::read_meta::<String>(connection, key) {
        Ok(value) => Reading::Value(value),
        Err(error) => match norn_db::damage_or_fail("reading the sidecar schema", error) {
            Ok(detail) => Reading::Damaged(detail),
            Err(refused) => Reading::Refused(refused.into()),
        },
    }
}
