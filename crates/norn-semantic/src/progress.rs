//! How far the engine has got, in the two senses a reader needs apart.
//!
//! **Freshness is not identity.** A [`Watermark`] says how far one feed was
//! drained against the lane-1 store: the store generation observed when that
//! feed last completed. A [`SidecarRevision`] says which sidecar state an
//! answer came from: the sidecar's epoch and how many committed mutations it
//! had seen. Two drains that each find nothing new leave both where they were;
//! a drain that advances a watermark over an otherwise empty feed moves the
//! revision too, because recording the watermark is itself a committed
//! mutation. A reader comparing two answers compares revisions; a reader
//! judging how stale an answer is compares watermarks against the store.
//!
//! Both are held by the engine and read through `&self`, so an owner that
//! serializes the engine's drains against its answers — the host's slot lock —
//! samples them in the same critical section as the answer they describe.

use norn_db::rusqlite::Connection;

use crate::ddl;
use crate::error::EngineError;

/// How far one feed was drained: the lane-1 store epoch and write generation
/// observed immediately before the page that completed it.
///
/// The generation is the store's, not the feed's: a store write that presents
/// no feed row still moves it, so an empty feed advances its watermark. Read
/// before the completing page rather than after it, so every write at or below
/// it was committed before that page was read and is covered by it — a write
/// landing during the drain lands above the watermark and reads as lag.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Watermark {
    /// The store lifetime the generation is a position in.
    pub store_epoch: String,
    /// The store's last committed write generation at that moment.
    pub generation: i64,
}

/// The two feeds' watermarks, each `None` until that feed first completes a
/// drain in any store lifetime.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Watermarks {
    /// The document feed's watermark.
    pub documents: Option<Watermark>,
    /// The tombstone feed's watermark.
    pub tombstones: Option<Watermark>,
}

/// Which sidecar state an answer was taken from.
///
/// The pair, never the bare revision, is the identity: a sidecar rebuild mints
/// a new epoch and starts its revision over, so a revision compared across two
/// epochs compares nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SidecarRevision {
    /// The sidecar's own epoch — see [`crate::Engine::epoch`].
    pub epoch: String,
    /// How many committed mutations the sidecar has seen within that epoch:
    /// every drained page, every reconcile, and every watermark advance takes
    /// one, and a drain that commits nothing takes none.
    pub revision: i64,
}

/// Which of the two feeds a watermark belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Feed {
    Documents,
    Tombstones,
}

impl Feed {
    /// The sidecar key the feed's watermark is recorded under.
    pub(crate) fn watermark_key(self) -> &'static str {
        match self {
            Feed::Documents => ddl::meta::DOCUMENT_WATERMARK,
            Feed::Tombstones => ddl::meta::TOMBSTONE_WATERMARK,
        }
    }
}

impl Watermarks {
    /// The feed's watermark.
    pub(crate) fn of(&self, feed: Feed) -> Option<&Watermark> {
        match feed {
            Feed::Documents => self.documents.as_ref(),
            Feed::Tombstones => self.tombstones.as_ref(),
        }
    }

    /// Record `watermark` as the feed's.
    pub(crate) fn set(&mut self, feed: Feed, watermark: Watermark) {
        match feed {
            Feed::Documents => self.documents = Some(watermark),
            Feed::Tombstones => self.tombstones = Some(watermark),
        }
    }
}

/// `{generation}:{store epoch}` — one scalar, so a watermark moves with its
/// epoch atomically. The generation half cannot hold `:`, so the first `:` is
/// always the seam, and the epoch keeps whatever it holds.
pub(crate) fn encode_watermark(watermark: &Watermark) -> String {
    format!("{}:{}", watermark.generation, watermark.store_epoch)
}

/// The recorded watermark back into its halves, or the reason it is not one
/// this build wrote.
fn decode_watermark(key: &str, recorded: &str) -> Result<Watermark, String> {
    let Some((generation, store_epoch)) = recorded.split_once(':') else {
        return Err(format!(
            "the recorded `{key}` value `{recorded}` has no separator"
        ));
    };
    let generation: i64 = generation
        .parse()
        .map_err(|_| format!("the recorded `{key}` value `{recorded}` has no generation"))?;
    if store_epoch.is_empty() {
        return Err(format!(
            "the recorded `{key}` value `{recorded}` names no store epoch"
        ));
    }
    Ok(Watermark {
        store_epoch: store_epoch.to_string(),
        generation,
    })
}

/// What the sidecar records of its own progress: its revision and the two
/// watermarks.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Recorded {
    pub(crate) revision: i64,
    pub(crate) watermarks: Watermarks,
}

/// Why the recorded progress could not be read.
pub(crate) enum Unread {
    /// A key holds something no reader of this build wrote — or the revision,
    /// which every sidecar this build creates records, is absent — named as
    /// the detail a rebuild carries.
    Damaged(String),
    /// The environment refused the read.
    Refused(norn_db::DbError),
}

/// Read the sidecar's recorded progress.
///
/// The revision is the sidecar's write generation, seeded at create; one that
/// is absent says the file was written by a build that did not count its
/// mutations, and resolves the way every other shape mismatch does.
pub(crate) fn read(connection: &Connection) -> Result<Recorded, Unread> {
    let revision =
        match norn_db::meta::read_meta::<i64>(connection, norn_db::meta::WRITE_GENERATION) {
            Ok(Some(revision)) => revision,
            Ok(None) => {
                return Err(Unread::Damaged(
                    "the sidecar records no revision".to_string(),
                ));
            }
            Err(error) => return Err(classify(error)),
        };
    let mut watermarks = Watermarks::default();
    for feed in [Feed::Documents, Feed::Tombstones] {
        let key = feed.watermark_key();
        match norn_db::meta::read_meta::<String>(connection, key) {
            Ok(None) => {}
            Ok(Some(recorded)) => {
                watermarks.set(
                    feed,
                    decode_watermark(key, &recorded).map_err(Unread::Damaged)?,
                );
            }
            Err(error) => return Err(classify(error)),
        }
    }
    Ok(Recorded {
        revision,
        watermarks,
    })
}

/// A driver refusal of a progress read, split by the substrate's one damage
/// policy for pinned state: a value the column cannot produce is part of the
/// shape, and a busy database or an I/O error is the environment.
fn classify(error: norn_db::rusqlite::Error) -> Unread {
    match norn_db::damage_or_fail("reading the sidecar's progress", error) {
        Ok(detail) => Unread::Damaged(detail),
        Err(refused) => Unread::Refused(refused),
    }
}

impl Unread {
    /// The refusal an engine reports where the progress it is loading will not
    /// read.
    pub(crate) fn into_engine_error(self) -> EngineError {
        match self {
            Unread::Damaged(what) => EngineError::SidecarDamaged { what },
            Unread::Refused(error) => error.into(),
        }
    }
}
