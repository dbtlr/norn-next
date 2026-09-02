//! The model gate: the verdict the sidecar takes over its own pinned keys,
//! once the mechanics have called the database usable.

use norn_semantic::{RebuildReason, SidecarOutcome};

use crate::common::{CountingEmbedder, Scratch};

/// Write `statement` on a direct connection to the sidecar, outside every
/// handle the engine holds.
fn out_of_band(path: &std::path::Path, statement: &str) {
    match norn_db::connect(path).expect("connecting to the sidecar") {
        norn_db::Attempt::Connected(connection) => {
            connection.execute(statement, []).expect(statement);
        }
        norn_db::Attempt::Unreadable { detail } => panic!("the sidecar is unreadable: {detail}"),
    }
}

/// A recorded model key the sidecar's own read cannot make sense of is a
/// rebuild, not a refusal.
///
/// The mechanics call the database usable — the version, the fingerprint, the
/// schema digest and the epoch all agree — and the value under the model key is
/// still one no reader of this build ever wrote. That is a statement about the
/// file's contents, so it resolves the way every other one does: a fresh
/// sidecar and a full recompute.
#[test]
fn a_model_key_that_will_not_read_is_rebuilt_from_zero() {
    let scratch = Scratch::new("model-unreadable");
    let engine = scratch.engine(CountingEmbedder::new());
    assert_eq!(engine.open_outcome(), &SidecarOutcome::Created);
    drop(engine);

    out_of_band(
        &scratch.sidecar_path(),
        "UPDATE meta SET value = 42 WHERE key = 'engine_model_id'",
    );

    let engine = scratch.engine(CountingEmbedder::new());
    match engine.open_outcome() {
        SidecarOutcome::RebuiltFromZero(RebuildReason::Client { detail }) => {
            assert!(!detail.is_empty(), "the rebuild named nothing it found");
        }
        other => panic!("the sidecar opened as {other:?} rather than rebuilding"),
    }
    assert_eq!(engine.projection().expect("a projection"), Vec::new());
}
