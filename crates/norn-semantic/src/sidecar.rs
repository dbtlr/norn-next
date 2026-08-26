//! Opening the sidecar: adopt the database at the path, or rebuild it.
//!
//! The ceremony is the same one every norn database runs — connect, read the
//! pinned scalars, compare the DDL fingerprint and the schema digest, and
//! either adopt what is there or remove it and create from zero — spelled
//! over this crate's schema. The substrate ([`norn_db`]) owns the pieces
//! (connection, meta, digests, damage typing, file lifecycle); this module
//! owns what they mean for the sidecar.
//!
//! A sidecar rebuild is always safe: every row is a projection of lane-1
//! records and the feed cursors reset with the epoch, so the next drain
//! recomputes exactly what a fresh sidecar is missing. That is why every
//! verdict short of a broken environment resolves to a rebuild rather than a
//! refusal.

use std::path::Path;

use norn_db::rusqlite::Connection;
use norn_db::{Attempt, DbError};
use norn_embed::Model;

use crate::ddl;
use crate::error::EngineError;

/// How an open ended up with this sidecar.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SidecarOutcome {
    /// No sidecar stood at the path; one was created from zero.
    Created,
    /// The sidecar at the path is the shape this build writes, and it was
    /// adopted with every row and recorded cursor intact.
    Reused,
    /// A database stood at the path and was not one this build wrote — a
    /// moved fingerprint, a foreign file, damage. It was removed and created
    /// from zero; the detail says what was found.
    RebuiltFromZero { detail: String },
}

/// What inspecting a connected database concluded.
enum Verdict {
    /// No schema at all: a new file, or an empty one.
    Fresh,
    /// The shape this build writes.
    Usable,
    /// Not a sidecar this build wrote; remove it and create from zero.
    Rebuild { detail: String },
}

/// Open, or create, the sidecar at `path` for `model`.
pub(crate) fn open(
    path: &Path,
    model: &Model,
) -> Result<(Connection, SidecarOutcome), EngineError> {
    norn_db::prepare_parent(path)?;
    match norn_db::connect(path)? {
        Attempt::Connected(connection) => match inspect(&connection, model)? {
            Verdict::Fresh => {
                create(&connection, model)?;
                Ok((connection, SidecarOutcome::Created))
            }
            Verdict::Usable => Ok((connection, SidecarOutcome::Reused)),
            Verdict::Rebuild { detail } => {
                drop(connection);
                Ok((
                    rebuild(path, model)?,
                    SidecarOutcome::RebuiltFromZero { detail },
                ))
            }
        },
        Attempt::Unreadable { detail } => Ok((
            rebuild(path, model)?,
            SidecarOutcome::RebuiltFromZero { detail },
        )),
    }
}

/// Read the schema this database carries, and decide whether it is the one
/// this build writes.
///
/// Every read here fails the same way: through [`rebuild_or_fail`], so one
/// policy decides whether a driver error describes a damaged file or a broken
/// environment — a busy database or an I/O error is not evidence that the
/// stored state is wrong.
fn inspect(connection: &Connection, model: &Model) -> Result<Verdict, EngineError> {
    let objects: i64 =
        match connection.query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0)) {
            Ok(count) => count,
            Err(error) => return rebuild_or_fail(error),
        };
    if objects == 0 {
        return Ok(Verdict::Fresh);
    }

    let has_meta: i64 = match connection.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name = 'meta'",
        [],
        |row| row.get(0),
    ) {
        Ok(count) => count,
        Err(error) => return rebuild_or_fail(error),
    };
    if has_meta == 0 {
        return Ok(Verdict::Rebuild {
            detail: "the database holds tables and no `meta` table, so it is not a sidecar this \
                     build wrote"
                .to_string(),
        });
    }

    let version: Option<i64> =
        match norn_db::meta::read_meta(connection, norn_db::meta::STORE_SCHEMA_VERSION) {
            Ok(version) => version,
            Err(error) => return rebuild_or_fail(error),
        };
    if version != Some(ddl::ENGINE_SCHEMA_VERSION) {
        return Ok(Verdict::Rebuild {
            detail: format!(
                "the sidecar records schema version {version:?} and this build writes {}",
                ddl::ENGINE_SCHEMA_VERSION
            ),
        });
    }

    let expected = ddl::fingerprint();
    let found: Option<String> =
        match norn_db::meta::read_meta(connection, norn_db::meta::DDL_FINGERPRINT) {
            Ok(found) => found,
            Err(error) => return rebuild_or_fail(error),
        };
    if found.as_deref() != Some(expected.as_str()) {
        return Ok(Verdict::Rebuild {
            detail: format!(
                "the sidecar records DDL fingerprint {} and this build writes {expected}",
                found.as_deref().unwrap_or("nothing")
            ),
        });
    }

    let recorded: Option<String> =
        match norn_db::meta::read_meta(connection, norn_db::meta::SCHEMA_DIGEST) {
            Ok(recorded) => recorded,
            Err(error) => return rebuild_or_fail(error),
        };
    let holds = match norn_db::schema_digest(connection) {
        Ok(digest) => digest,
        Err(error) => return rebuild_or_fail(error),
    };
    if recorded.as_deref() != Some(holds.as_str()) {
        return Ok(Verdict::Rebuild {
            detail: format!(
                "the database holds a schema digesting {holds} and it was created holding {}, so \
                 an object it was created with has been changed or removed",
                recorded.as_deref().unwrap_or("nothing recorded")
            ),
        });
    }

    // The epoch is the last gate because the three above cannot ask it:
    // create writes one, so a database that agrees about its whole shape and
    // records no epoch was written by something else.
    let epoch: Option<String> =
        match norn_db::meta::read_meta(connection, norn_db::meta::STORE_EPOCH) {
            Ok(epoch) => epoch,
            Err(error) => return rebuild_or_fail(error),
        };
    if epoch.is_none() {
        return Ok(Verdict::Rebuild {
            detail: "the database records no epoch, so it is not a sidecar this build wrote"
                .to_string(),
        });
    }

    // The model last: a shape this build wrote, holding another model's rows
    // and cursors. The cursors are model-blind — the drained feed is drained
    // for whichever model was embedding — so adopting them under a new model
    // would skip everything the old one consumed. A moved model is resolved
    // by the wholesale-rebuild floor: fresh sidecar, full recompute.
    let recorded_id: Option<String> =
        match norn_db::meta::read_meta(connection, ddl::meta::ENGINE_MODEL_ID) {
            Ok(id) => id,
            Err(error) => return rebuild_or_fail(error),
        };
    let recorded_version: Option<String> =
        match norn_db::meta::read_meta(connection, ddl::meta::ENGINE_MODEL_VERSION) {
            Ok(version) => version,
            Err(error) => return rebuild_or_fail(error),
        };
    if recorded_id.as_deref() != Some(model.id())
        || recorded_version.as_deref() != Some(model.version())
    {
        return Ok(Verdict::Rebuild {
            detail: format!(
                "the sidecar was written for model {}/{} and this engine embeds with {}/{}",
                recorded_id.as_deref().unwrap_or("nothing"),
                recorded_version.as_deref().unwrap_or("nothing"),
                model.id(),
                model.version(),
            ),
        });
    }
    Ok(Verdict::Usable)
}

/// Which rung a driver error met while reading the sidecar schema belongs to:
/// a rebuild verdict where the file's own contents are wrong, or the refused
/// operation the environment made of it.
fn rebuild_or_fail(error: norn_db::rusqlite::Error) -> Result<Verdict, EngineError> {
    match norn_db::damage_or_fail("reading the sidecar schema", error) {
        Ok(detail) => Ok(Verdict::Rebuild { detail }),
        Err(refused) => Err(EngineError::from(refused)),
    }
}

/// Remove the database and create it again from the statement list.
pub(crate) fn rebuild(path: &Path, model: &Model) -> Result<Connection, EngineError> {
    norn_db::remove_database(path)?;
    match norn_db::connect(path)? {
        Attempt::Connected(connection) => {
            create(&connection, model)?;
            Ok(connection)
        }
        Attempt::Unreadable { detail } => Err(EngineError::from(DbError::Damaged {
            what: format!("a sidecar rebuilt from zero is still not readable: {detail}"),
        })),
    }
}

/// Run the whole statement list, and record what was run.
///
/// One transaction: a half-created schema is one nothing can inspect, and the
/// pinned values written at the end are what say the list finished. The
/// schema digest is taken after the statements ran, so it is a reading of the
/// database rather than a second reading of the list.
fn create(connection: &Connection, model: &Model) -> Result<(), EngineError> {
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| crate::error::sql("opening the sidecar schema transaction", error))?;
    for statement in ddl::statements() {
        transaction.execute_batch(&statement).map_err(|error| {
            EngineError::from(norn_db::sql_at_statement(
                "creating the sidecar schema",
                &statement,
                error,
            ))
        })?;
    }
    let digest = norn_db::schema_digest(&transaction)
        .map_err(|error| crate::error::sql("digesting the sidecar schema", error))?;
    norn_db::meta::put_meta(
        &transaction,
        norn_db::meta::STORE_SCHEMA_VERSION,
        ddl::ENGINE_SCHEMA_VERSION,
    )?;
    norn_db::meta::put_meta(
        &transaction,
        norn_db::meta::DDL_FINGERPRINT,
        ddl::fingerprint(),
    )?;
    norn_db::meta::put_meta(&transaction, norn_db::meta::SCHEMA_DIGEST, digest)?;
    norn_db::meta::put_meta(&transaction, ddl::meta::ENGINE_MODEL_ID, model.id())?;
    norn_db::meta::put_meta(
        &transaction,
        ddl::meta::ENGINE_MODEL_VERSION,
        model.version(),
    )?;
    norn_db::meta::put_meta(
        &transaction,
        norn_db::meta::STORE_EPOCH,
        norn_db::mint_an_epoch(&transaction)?,
    )?;
    transaction
        .commit()
        .map_err(|error| crate::error::sql("committing the sidecar schema", error))
}
