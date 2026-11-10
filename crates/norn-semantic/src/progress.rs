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
use norn_store::StoreReading;

use crate::census::{Counts, Coverage};
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
    /// The store epoch the generation is a position in.
    pub store_epoch: String,
    /// The store's last committed write generation at that moment.
    pub generation: i64,
}

/// The two feeds' watermarks, each `None` until that feed first completes a
/// drain in any store epoch.
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

/// A write generation at or below which one reader of the death feed has read
/// every death of one store epoch, or accounted for it by a reconcile, with
/// the store epoch it is a position in ([ADR 0052]).
///
/// It is the highest of what the reader has recorded in that epoch: the
/// generation before its death cursor's own, its last completed drain of the
/// death feed (the engine's tombstone watermark) or count of both feeds (the
/// census's completion), and the G0 its last completed reconcile or recount
/// read on its first page. While a reconcile or recount runs, its death cursor
/// counts toward none of these, and the value is the one its records gave
/// when it began.
///
/// [ADR 0052]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0052-a-death-is-kept-only-while-lane-2-or-a-broken-link-needs-it.md
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadThroughGeneration {
    /// The store epoch the generation is a position in.
    pub store_epoch: String,
    /// Every death at or below this generation was read or accounted for.
    pub generation: i64,
}

/// The read-through generations of the engine's two readers of the death
/// feed: the drain and the census. Each is `None` where that reader has
/// recorded nothing, which reads as having read no death.
///
/// The death prune is handed the lower of the two
/// ([`ReadThroughs::lower_in`]), and a judgment of the engine's freshness
/// against a store reading asks whether the engine has read through that
/// reading's floor ([`ReadThroughs::engine_below_floor`]).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReadThroughs {
    /// The drain's: how far the engine's retractions have read.
    pub engine: Option<ReadThroughGeneration>,
    /// The census's: how far the coverage counts have read.
    pub census: Option<ReadThroughGeneration>,
}

impl ReadThroughs {
    /// The engine's read-through generation in `store_epoch`: `None` where
    /// it recorded none there, which orders below every generation.
    pub fn engine_in(&self, store_epoch: &str) -> Option<i64> {
        in_epoch(self.engine.as_ref(), store_epoch)
    }

    /// The census's read-through generation in `store_epoch`.
    pub fn census_in(&self, store_epoch: &str) -> Option<i64> {
        in_epoch(self.census.as_ref(), store_epoch)
    }

    /// The lower of the two readers' read-through generations in
    /// `store_epoch`, which is what the death prune is handed once the host
    /// maps it: `None` where either has read nothing there.
    pub fn lower_in(&self, store_epoch: &str) -> Option<i64> {
        self.engine_in(store_epoch).min(self.census_in(store_epoch))
    }

    /// Whether the engine has not read through the death floor `reading`
    /// carries: its read-through generation in the reading's store epoch
    /// stands below the floor. A store with no floor is read through by
    /// every reader.
    pub fn engine_below_floor(&self, reading: &StoreReading) -> bool {
        below(self.engine_in(reading.epoch()), reading.death_floor())
    }
}

/// `recorded` as a generation of `store_epoch`: `None` where it is absent or
/// a position in another epoch.
pub(crate) fn in_epoch(recorded: Option<&ReadThroughGeneration>, store_epoch: &str) -> Option<i64> {
    recorded
        .filter(|recorded| recorded.store_epoch == store_epoch)
        .map(|recorded| recorded.generation)
}

/// Whether a reader at `read_through` has not read through `floor`. One
/// judgment, so the drain, the census and the host ask it the same way.
pub(crate) fn below(read_through: Option<i64>, floor: Option<i64>) -> bool {
    floor.is_some_and(|floor| read_through < Some(floor))
}

/// A reader's read-through generation as the sidecar records it beside a
/// reconcile or a recount: `{generation}:{store epoch}`, or the empty string
/// where it is none.
pub(crate) fn encode_read_through(read_through: Option<&ReadThroughGeneration>) -> String {
    read_through.map_or_else(String::new, |read_through| {
        encode_position(read_through.generation, &read_through.store_epoch)
    })
}

/// A generation recorded beside the store epoch it is a position in, as the
/// sidecar records a completed reconcile, a recount or a census completion.
pub(crate) fn encode_generation_in(generation: i64, store_epoch: &str) -> String {
    encode_position(generation, store_epoch)
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
    encode_position(watermark.generation, &watermark.store_epoch)
}

/// A generation and the store epoch it is a position in, as one scalar.
fn encode_position(generation: i64, store_epoch: &str) -> String {
    format!("{generation}:{store_epoch}")
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

/// What the sidecar records of its own progress: its revision, the two
/// watermarks and the census counts.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Recorded {
    pub(crate) revision: i64,
    pub(crate) watermarks: Watermarks,
    /// The census counts, and whether they are whole, as the last commit left
    /// them.
    pub(crate) counts: Counts,
    /// The two readers' read-through generations, as the last commit left
    /// their records.
    pub(crate) read_through: ReadThroughs,
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
    let mut counts = Counts::default();
    for (key, count) in [
        (ddl::meta::CENSUS_TOTAL, &mut counts.total),
        (ddl::meta::CENSUS_EMBEDDED, &mut counts.embedded),
    ] {
        match norn_db::meta::read_meta::<i64>(connection, key) {
            Ok(Some(recorded)) if recorded >= 0 => *count = recorded,
            Ok(Some(recorded)) => {
                return Err(Unread::Damaged(format!(
                    "the recorded `{key}` count {recorded} is below zero"
                )));
            }
            Ok(None) => {
                return Err(Unread::Damaged(format!(
                    "the sidecar records no `{key}` count"
                )));
            }
            Err(error) => return Err(classify(error)),
        }
    }
    let key = ddl::meta::CENSUS_WHOLE;
    counts.whole = match norn_db::meta::read_meta::<i64>(connection, key) {
        Ok(Some(0)) => false,
        Ok(Some(1)) => true,
        Ok(Some(recorded)) => {
            return Err(Unread::Damaged(format!(
                "the recorded `{key}` value {recorded} is neither 0 nor 1"
            )));
        }
        Ok(None) => {
            return Err(Unread::Damaged(format!(
                "the sidecar records no `{key}` value"
            )));
        }
        Err(error) => return Err(classify(error)),
    };
    counts.last_whole = read_last_whole(connection)?;
    Ok(Recorded {
        revision,
        watermarks,
        counts,
        read_through: read_throughs(connection)?,
    })
}

/// The meta keys one reader of the death feed records its read-through
/// generation's parts under.
struct ReaderKeys {
    /// The store epoch the reader's death cursor is a position in.
    epoch: &'static str,
    /// The reader's death cursor, `{generation}:{path}` or empty.
    death_cursor: &'static str,
    /// The reader's last completed read of the death feed, as a position.
    completed: &'static str,
    /// The G0 of the reader's last completed reconcile or recount.
    reconciled: &'static str,
    /// The running reconcile's or recount's G0, an integer present exactly
    /// while one runs.
    running: &'static str,
    /// The read-through generation stored with the running reconcile.
    frozen: &'static str,
}

const ENGINE_KEYS: ReaderKeys = ReaderKeys {
    epoch: ddl::meta::OBSERVED_STORE_EPOCH,
    death_cursor: ddl::meta::TOMBSTONE_CURSOR,
    completed: ddl::meta::TOMBSTONE_WATERMARK,
    reconciled: ddl::meta::RECONCILED_THROUGH,
    running: ddl::meta::RECONCILE_G0,
    frozen: ddl::meta::RECONCILE_READ_THROUGH,
};

const CENSUS_KEYS: ReaderKeys = ReaderKeys {
    epoch: ddl::meta::CENSUS_STORE_EPOCH,
    death_cursor: ddl::meta::CENSUS_TOMBSTONE_CURSOR,
    completed: ddl::meta::CENSUS_COMPLETION,
    reconciled: ddl::meta::CENSUS_RECOUNTED,
    running: ddl::meta::CENSUS_RECOUNT_G0,
    frozen: ddl::meta::CENSUS_RECOUNT_READ_THROUGH,
};

/// The two readers' read-through generations, as the sidecar records them.
///
/// Read inside every transaction that moves a reader's records, before it
/// commits, so the engine's reading moves with what it describes.
pub(crate) fn read_throughs(connection: &Connection) -> Result<ReadThroughs, Unread> {
    Ok(ReadThroughs {
        engine: reader_read_through(connection, &ENGINE_KEYS)?,
        census: reader_read_through(connection, &CENSUS_KEYS)?,
    })
}

/// One reader's read-through generation: the value stored with its running
/// reconcile, or else the highest of its recorded parts that are positions in
/// the store epoch its death cursor is valid in.
fn reader_read_through(
    connection: &Connection,
    keys: &ReaderKeys,
) -> Result<Option<ReadThroughGeneration>, Unread> {
    let running = norn_db::meta::read_meta::<i64>(connection, keys.running).map_err(classify)?;
    if running.is_some() {
        let frozen = read_text(connection, keys.frozen)?.unwrap_or_default();
        if frozen.is_empty() {
            return Ok(None);
        }
        return decode_read_through(keys.frozen, &frozen).map(Some);
    }
    let Some(epoch) = read_text(connection, keys.epoch)?.filter(|epoch| !epoch.is_empty()) else {
        return Ok(None);
    };
    let mut highest: Option<i64> = None;
    if let Some(cursor) = read_text(connection, keys.death_cursor)?.filter(|c| !c.is_empty()) {
        // The generation before the cursor's own: deaths of the cursor's
        // generation past its path are still unread.
        highest = Some(cursor_generation(keys.death_cursor, &cursor)? - 1);
    }
    for key in [keys.completed, keys.reconciled] {
        if let Some(recorded) = read_text(connection, key)? {
            let recorded = decode_read_through(key, &recorded)?;
            if recorded.store_epoch == epoch {
                highest = highest.max(Some(recorded.generation));
            }
        }
    }
    Ok(highest.map(|generation| ReadThroughGeneration {
        store_epoch: epoch,
        generation,
    }))
}

/// A recorded `{generation}:{store epoch}` read-through part.
fn decode_read_through(key: &str, recorded: &str) -> Result<ReadThroughGeneration, Unread> {
    let watermark = decode_watermark(key, recorded).map_err(Unread::Damaged)?;
    Ok(ReadThroughGeneration {
        store_epoch: watermark.store_epoch,
        generation: watermark.generation,
    })
}

/// The generation half of a recorded `{generation}:{path}` feed position.
fn cursor_generation(key: &str, recorded: &str) -> Result<i64, Unread> {
    recorded
        .split_once(':')
        .and_then(|(generation, _)| generation.parse().ok())
        .ok_or_else(|| {
            Unread::Damaged(format!(
                "the recorded `{key}` position `{recorded}` has no generation"
            ))
        })
}

/// One of the sidecar's text scalars.
fn read_text(connection: &Connection, key: &str) -> Result<Option<String>, Unread> {
    norn_db::meta::read_meta::<String>(connection, key).map_err(classify)
}

/// The last whole count, recorded as both of its counts or neither.
fn read_last_whole(connection: &Connection) -> Result<Option<Coverage>, Unread> {
    let read = |key: &str| match norn_db::meta::read_meta::<i64>(connection, key) {
        Ok(None) => Ok(None),
        Ok(Some(recorded)) => u64::try_from(recorded).map(Some).map_err(|_| {
            Unread::Damaged(format!(
                "the recorded `{key}` count {recorded} is below zero"
            ))
        }),
        Err(error) => Err(classify(error)),
    };
    match (
        read(ddl::meta::CENSUS_LAST_WHOLE_TOTAL)?,
        read(ddl::meta::CENSUS_LAST_WHOLE_EMBEDDED)?,
    ) {
        (Some(total), Some(embedded)) => Ok(Some(Coverage { embedded, total })),
        (None, None) => Ok(None),
        _ => Err(Unread::Damaged(
            "the sidecar records half of a last whole count".to_string(),
        )),
    }
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
