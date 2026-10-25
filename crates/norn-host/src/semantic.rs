//! The semantic engines the host composes: one per enabled vault.
//!
//! This is the crate-map edge `norn-host → norn-semantic`, and the whole of
//! it. The host owns three things here and defines nothing past them:
//!
//! - **Delivery records what the section asks for, and opens nothing.** The
//!   config dispatch hands this set the vault's `[engine.semantic]` section;
//!   a section that enables the engine leaves a preparing slot (or keeps the
//!   running or preparing one it already has), an absent section (or
//!   `enabled = false`) closes it, and a section the engine refuses leaves a
//!   typed self-disabled state a status read reports. The sidecar open and
//!   the model setup — the initialization — run later, as a lifecycle slice
//!   after the publication that committed the delivery, and an engine that
//!   cannot open is self-disabled there.
//!   What the section *means* is the engine's own ([`norn_semantic::Settings`]).
//!   The reading of the section itself — absent, disabled, malformed with the
//!   engine's refusal, or enabled — is retained beside the slot at the same
//!   delivery ([`SemanticEngines::section`]), and a refusal that no engine
//!   stands carries the reading taken under the same lookup as the slot it
//!   found missing, because "no engine stands" is answered differently for a
//!   vault that asked for none and one whose section could not be read.
//! - **The nudge is owed work, run in bounded slices.** Every leg that ends
//!   holding a consistent lane-1 store — the increments, every heal (attach,
//!   recover, schema reload, and the store rebuild whose new epoch is exactly
//!   what a cursor must not sleep through), and the config-only reload, whose
//!   drain is what converges a corpus derived before its engine existed —
//!   records that the slot owes a slice ([`SemanticEngines::owe`]) and runs
//!   none of it, so its publication never waits for semantic work. Quiet
//!   maintenance legs then run one unit each over the leg's own feed handle
//!   ([`SemanticEngines::work`]): no thread, no channel, no missable wake —
//!   a dedicated engine worker is the carve drain cost forces, not this
//!   composition's obligation. A drain failure never fails the leg: lane 2's
//!   contract is eventual consistency, so engine trouble is retained as the
//!   slot's own diagnostic, and sidecar damage resolves by the engine's
//!   rebuild floor, whose reset cursors the next slices drain.
//! - **Answers come from the slot, gated by the slot alone.** A nearest
//!   answer needs the engine and its sidecar, never the vault's store, so it
//!   runs on the caller's thread and answers whenever a slot stands — the
//!   vault's trust label does not gate the capability. A status reads the
//!   report kept beside the slot rather than the slot itself, through the
//!   delivery the vault's entry committed with the fingerprints it publishes
//!   ([`DeliveredEngine`]), so it never reads this set. The `search` verb
//!   composes it through the read seam, so a search's vector rung answers only
//!   for an entry the seam serves, beside the snapshot its hold established.
//! - **An answer carries its own reading.** A nearest answer is sampled under
//!   the slot's lock together with the model it ran, the sidecar revision it
//!   was taken from and the engine's watermarks, so no drain lands between
//!   the rows and the reading that describes them. Freshness is those
//!   watermarks judged against the store reading the caller's own hold
//!   established ([`freshness`]).
//!
//! # Locking, and the guarantee it leans on
//!
//! The outer map lock covers lookup, delivery bookkeeping, publication and
//! teardown only — never an engine open, a sidecar close, a drain, or an
//! answer — so one vault's work never holds another vault's. The one place
//! it is held with a slot's lock is an initialization's install, which moves
//! an already-open engine into a preparing slot and does no I/O. It is taken
//! after the entry gate where both are held, never before it. Within one vault, a slot's own lock serializes its
//! drains and its answers against each other, and a drain holds it for the
//! whole drain. The report beside each slot has a lock of its own, written
//! while the slot's lock is held and read without it, so a status taken
//! during a drain reads the slot as it stood before that drain rather than
//! waiting for it to end ([`EngineSlot`]). Slot replacement at delivery
//! is sound because the lifecycle admits one leg per vault at a time (the
//! entry's custody claim); a lifecycle that relaxed that would need delivery
//! to mutate in place rather than replace. An initialization does not lean
//! on that alone: its result is installed only into the slot it opened for,
//! while that slot is still the vault's, beside the store lifetime its
//! publication committed, under a claim that still stands.
//!
//! Every slot lock tolerates poison: a panic that escaped the engine never
//! becomes a second panic on a worker leg or a caller's thread. What it leaves
//! depends on where it escaped — an engine borrowed for an ordinary drain or a
//! nearest answer stays in the slot, and an engine taken out for the
//! sidecar-damage rebuild is abandoned, which every later reader answers as
//! the self-disabled state.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use norn_config::ConfigDirs;
use norn_config::vault::EngineConfig;
use norn_embed::{Model, StubEmbedder};
use norn_semantic::{
    Engine, NearestWork, Neighbor, Settings, SidecarRevision, SliceBudget, Watermark, Watermarks,
};
use norn_store::{FeedRead, StoreReading};
use norn_wire::{
    EngineSection, EngineStatus, ErrorDetail, ErrorEnvelope, Freshness, Rung, RungSkipReason,
    VaultName,
};

use crate::refusal::engine_refusal_told;
use crate::reload::EngineConfigReceiver;

/// The sidecar's file, beside the vault's `store.sqlite3` in the same
/// derived directory.
pub(crate) const SIDECAR_FILE: &str = "semantic.sqlite3";

/// One vault's engine, or the typed reason it is not running.
enum Slot {
    /// The delivered section enables the engine and none is open yet. The
    /// sidecar open and the model setup are owed: they run as a lifecycle
    /// slice once the publication that committed this delivery has been
    /// made ([`SemanticEngines::work`]), never at the delivery itself.
    Preparing,
    Running {
        /// `None` only when a panic escaped the sidecar-damage rebuild, which
        /// takes the engine out of the slot to discard and reopen it; every
        /// reader maps that reading to a self-disabled answer. A panic inside an
        /// ordinary drain or a nearest answer borrows, and leaves the engine here.
        ///
        /// Behind a box, so that a slot costs what a self-disabled one costs:
        /// an engine carries its open connection and its outcome, and every
        /// slot in the map would otherwise be that wide whether it holds an
        /// engine or a refusal.
        engine: Option<Box<Engine>>,
        /// The most recent drain's failure, cleared by the next drain that
        /// succeeds. Latency, never correctness: the rows the failed drain
        /// did not write are still owed by the cursor, and the next nudge
        /// re-attempts them.
        last_drain_error: Option<String>,
    },
    /// The engine took itself out of service: its section was refused, its
    /// sidecar could not open, a rebuild after damage failed, or a panic
    /// abandoned it. The next config delivery is what re-attempts it.
    SelfDisabled { detail: String },
}

/// The reading every abandoned-by-a-panic path reports.
const ABANDONED: &str = "a panic abandoned the engine mid-operation";

/// What a status read reports for one vault.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticStatus {
    /// No engine stands for this vault here and now: no delivered section
    /// enables one, or the vault is not attached. A slot fact, not an
    /// authoring fact — the serving surface composes the config beside it.
    Off,
    /// The delivered section enables an engine and it is not yet open: its
    /// initialization follows the publication that committed the section.
    Preparing,
    On {
        last_drain_error: Option<String>,
        /// The sidecar state the engine answers from now.
        sidecar: SidecarRevision,
        /// How far each feed was drained; judged against a store reading by
        /// [`freshness`].
        watermarks: Watermarks,
    },
    SelfDisabled {
        detail: String,
    },
}

/// One nearest answer and the reading it was taken under, sampled together
/// under the slot's lock.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticAnswer {
    /// The nearest paths, nearest first.
    pub neighbors: Vec<Neighbor>,
    /// The model the vectors were derived under and the query embedded with.
    pub model: Model,
    /// The sidecar state the rows came from.
    pub sidecar: SidecarRevision,
    /// How far each feed had been drained when the rows were read.
    pub watermarks: Watermarks,
    /// What the scan read, scored and held.
    pub work: NearestWork,
}

/// How far an engine's derived state trails `store`, the reading the caller's
/// own hold was established at.
///
/// Each feed's lag is the store's generation less the generation its watermark
/// recorded, and the answer is the worse of the two: an answer is as stale as
/// its stalest input. A feed whose watermark was taken in another store
/// lifetime — or that has not completed a drain in any — is rescanning, because
/// a generation compared across two databases compares nothing, and rescanning
/// is worse than any count. A watermark past the reading, recorded by a drain
/// that completed after the hold was established, trails by nothing.
///
/// The `search` handler composes it with the store reading of the hold its
/// answer is taken under; the report `vault status` and `doctor` give a
/// vault's engine is its other caller, with the store reading
/// the vault's entry recorded where its last leg's lane-1 work committed.
pub fn freshness(watermarks: &Watermarks, store: &StoreReading) -> Freshness {
    let lag = |watermark: &Option<Watermark>| match watermark {
        Some(watermark) if watermark.store_epoch == store.epoch() => {
            let behind = store
                .write_generation()
                .saturating_sub(watermark.generation);
            Some(u64::try_from(behind).unwrap_or(0))
        }
        Some(_) | None => None,
    };
    match (lag(&watermarks.documents), lag(&watermarks.tombstones)) {
        (Some(documents), Some(tombstones)) => Freshness::trailing(documents.max(tombstones)),
        _ => Freshness::rescanning(),
    }
}

/// Why a nearest answer was refused.
///
/// **This vocabulary stops at the host library.** Nothing here crosses a
/// wire: the serving surface that will carry semantic answers to a client
/// arrives with the verb charter, and its envelope mapping lands beside it
/// the way every host refusal's does (see `refusal.rs`). The variants are
/// typed so that mapping stays derivable rather than parsed from prose.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticRefusal {
    /// No engine stands for this vault. `section` is the reading the last
    /// config delivery left, taken in the same lookup that found no slot, or
    /// `None` where no delivery stands — the vault is not attached, or was
    /// detached since.
    NoEngine { section: Option<EngineSection> },
    /// The delivered section enables an engine whose initialization has not
    /// yet run.
    Preparing,
    /// The engine took itself out of service; the detail is the slot's.
    SelfDisabled { detail: String },
    /// The engine is running and this answer failed.
    Failed { detail: String },
}

impl std::fmt::Display for SemanticRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SemanticRefusal::NoEngine { .. } => {
                write!(f, "no semantic engine stands for this vault")
            }
            SemanticRefusal::Preparing => {
                write!(f, "the semantic engine is preparing")
            }
            SemanticRefusal::SelfDisabled { detail } => {
                write!(f, "the semantic engine is out of service: {detail}")
            }
            SemanticRefusal::Failed { detail } => {
                write!(f, "the semantic answer failed: {detail}")
            }
        }
    }
}

impl std::error::Error for SemanticRefusal {}

/// The refusal a vector rung answers with, composed from a nearest refusal:
/// the one composition every vector refusal a search meets goes through,
/// rendered as its envelope.
///
/// That no engine stands is answered by the delivered section it was found
/// beside: a vault whose section is absent or disabled is told what to enable
/// (`engine/not-enabled`), and every other reading, and an engine that took
/// itself out of service, is `engine/unavailable` carrying the section's own
/// error or the engine's retained diagnostic. An engine that stands and
/// failed its answer is `engine/failed`.
pub fn compose_vector_refusal(refusal: SemanticRefusal) -> ErrorEnvelope {
    VectorRefusal::of(refusal).envelope()
}

/// What a nearest refusal means for the vector rung of a search: the vault's
/// enabled set does not hold the rung, it holds it and no engine stands for
/// it, or an engine stands and the answer failed.
///
/// It is the one product a search's vector rung is refused through, whichever
/// selection named the rung: a selection naming the rung exactly is refused
/// with [`VectorRefusal::envelope`], and the enabled selection leaves a rung
/// its set does not hold out silently, skips an unavailable one with
/// [`VectorRefusal::skip_reason`], and is refused by a failed one. The read seam
/// serves the entry wherever this is reached: it refuses a vault it does not
/// serve with the ordinary answer-reading refusal before any rung runs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum VectorRefusal {
    /// The vault's enabled set does not hold the vector rung.
    NotEnabled { message: String, detail: String },
    /// The enabled set holds it, and no engine stands for it here and now.
    Unavailable { message: String, detail: String },
    /// An engine stands, and this answer failed.
    Failed { message: String, detail: String },
}

impl VectorRefusal {
    /// What `refusal` means for the vector rung.
    ///
    /// That no engine stands says nothing a client can act on by itself;
    /// whether the vault never enabled one is a fact about the delivered
    /// section, which the refusal carries from the lookup that found no slot.
    /// Composing it here is what lets a client be told what to do about it. A
    /// section that could not be read is delivered beside a self-disabled
    /// slot, so it answers as [`SemanticRefusal::SelfDisabled`], carrying the
    /// section's own error, rather than through this lookup.
    ///
    /// Both matches carry no wildcard, so a refusal or a section minted
    /// without a row here does not compile.
    pub(crate) fn of(refusal: SemanticRefusal) -> Self {
        match refusal {
            SemanticRefusal::NoEngine { section } => match section {
                // The vault has not asked for an engine, so there is nothing
                // unavailable — there is something to turn on.
                Some(EngineSection::Absent {} | EngineSection::Disabled {}) => {
                    VectorRefusal::NotEnabled {
                        message: "this vault answers no vector rung until its engine is enabled"
                            .to_string(),
                        detail:
                            "enable the engine section in .norn/config.toml and run vault reload"
                                .to_string(),
                    }
                }
                // A malformed section is delivered beside a self-disabled
                // slot, so a nearest refusal does not carry it with no engine;
                // the row answers what the section says, for the same reason
                // as the enabled row below.
                Some(EngineSection::Malformed { detail, .. }) => VectorRefusal::Unavailable {
                    message:
                        "this vault's engine section could not be read, so no engine stands for it"
                            .to_string(),
                    detail,
                },
                // An enabled section with no engine behind it is a slot the
                // delivery should have filled, and a delivery that reads an
                // enabled section always leaves one, so a nearest refusal does
                // not carry this pair. It is answered rather than panicked on,
                // because a wire mapping is not the place a host asserts its
                // own invariants.
                Some(EngineSection::Enabled {}) => VectorRefusal::Unavailable {
                    message: "this vault's engine is enabled and no engine stands for it"
                        .to_string(),
                    detail: "the engine slot is empty".to_string(),
                },
                // No delivery stands at all. A ready entry's delivery has
                // already happened, so a search does not reach this row; it is
                // answered for the same reason. A delivery never records the
                // undelivered reading, which is how a status renders the
                // absence of one, so it answers as that absence does.
                None | Some(EngineSection::Undelivered {}) => VectorRefusal::Unavailable {
                    message: "no engine section has been delivered to this vault".to_string(),
                    detail: "no engine section was delivered".to_string(),
                },
            },
            // The section enables the engine and its initialization is still
            // owed: unavailable for now, never disabled or failed.
            SemanticRefusal::Preparing => VectorRefusal::Unavailable {
                message: "this vault's engine is preparing, so it answers no vector rung yet"
                    .to_string(),
                detail: "the engine is preparing".to_string(),
            },
            // The engine took itself out of service. What the section says is
            // not the fact any more: the engine was delivered and stood down.
            SemanticRefusal::SelfDisabled { detail } => VectorRefusal::Unavailable {
                message: "this vault's engine is out of service, so it answers no vector rung"
                    .to_string(),
                detail,
            },
            // The engine stands and this answer failed.
            SemanticRefusal::Failed { detail } => VectorRefusal::Failed {
                message: "this vault's engine failed to answer the vector rung".to_string(),
                detail,
            },
        }
    }

    /// The vector rung of a host that composes no semantic engine: no vault it
    /// serves can enable one, so the enabled set holds no vector rung.
    pub(crate) fn not_composed() -> Self {
        VectorRefusal::NotEnabled {
            message: "this host answers no vector rung: it composes no semantic engine".to_string(),
            detail: "serve the vault from a host that composes the semantic engine".to_string(),
        }
    }

    /// The refusal a search naming the vector rung meets.
    pub(crate) fn envelope(self) -> ErrorEnvelope {
        match self {
            VectorRefusal::NotEnabled { message, detail } => ErrorEnvelope::new(
                message,
                ErrorDetail::engine_not_enabled(Rung::Vector, detail),
            ),
            VectorRefusal::Unavailable { message, detail } => ErrorEnvelope::new(
                message,
                ErrorDetail::engine_unavailable(Rung::Vector, detail),
            ),
            VectorRefusal::Failed { message, detail } => {
                ErrorEnvelope::new(message, ErrorDetail::engine_failed(Rung::Vector, detail))
            }
        }
    }

    /// Why the enabled selection skips the rung, where it skips it: only an
    /// enabled rung no engine stands for is skipped. A rung the enabled set
    /// does not hold is not in the selection to skip, and a failed answer is
    /// refused rather than skipped.
    pub(crate) fn skip_reason(&self) -> Option<RungSkipReason> {
        match self {
            VectorRefusal::Unavailable { detail, .. } => {
                Some(RungSkipReason::unavailable(detail.clone()))
            }
            VectorRefusal::NotEnabled { .. } | VectorRefusal::Failed { .. } => None,
        }
    }
}

/// What the last config delivery left for one vault: the section reading it
/// was delivered, and the slot that reading opened, where it opened one.
///
/// One entry under one lock, so a reader looking both up in one lookup sees
/// a pair one delivery produced.
struct Delivery {
    section: EngineSection,
    slot: Option<Arc<EngineSlot>>,
    /// The store lifetime the entry's publishing gate hold committed this
    /// delivery beside, or `None` until a hold has committed it. An owed
    /// initialization runs only after that publication, and only over the
    /// store lifetime it names ([`SemanticEngines::publish_delivery`]).
    published_store: Option<String>,
}

/// What one delivery left for a vault, as `vault status` and `doctor` report
/// it: the section reading, and the report cell of the slot it opened, where
/// it opened one.
///
/// **An entry commits one of these in the gate hold that publishes its
/// fingerprints**, and a status reads the committed one rather than the
/// delivery standing in this set. A leg delivers its config ahead of that
/// publication, so a status taken between the two reports the section and
/// the engine the published fingerprints were served beside, never the new
/// section beside the old fingerprints. It holds the report cell and not the
/// slot, so what the entry commits keeps no engine open: a slot a delivery
/// replaced closes when this set lets it go, and the cell it leaves reads as
/// that slot last stood.
#[derive(Clone, Debug)]
pub(crate) struct DeliveredEngine {
    section: EngineSection,
    report: Option<Arc<Mutex<SemanticStatus>>>,
}

impl DeliveredEngine {
    /// The section this delivery read, and the report its slot's last act
    /// left, or [`SemanticStatus::Off`] where it opened no slot. The slot's
    /// own lock is not taken, so this never waits behind a drain.
    pub(crate) fn reading(&self) -> (EngineSection, SemanticStatus) {
        let status = self
            .report
            .as_ref()
            .map_or(SemanticStatus::Off, |report| tolerant(report).clone());
        (self.section.clone(), status)
    }
}

/// One vault's engine slot, and the report of it a status reads.
///
/// **The report is read without the slot's lock.** A drain holds the slot's
/// lock for the whole drain, re-embedding included, and a status must not wait
/// behind it. So every act that changes the slot writes the report while it
/// still holds the slot — a delivery as it makes the slot, and a drain as its
/// hold ends, however it ends — and a status reads the report alone. What a
/// status reads is therefore the slot as the last act that held it left it:
/// during a drain, the slot as it stood before that drain.
///
/// **The slot's identity is the delivery's.** A delivery that keeps a slot
/// keeps this allocation, and one that replaces it makes another, so an
/// initialization holding this slot installs only while the map still holds
/// the same one: a detach and a re-attach under the same section and store
/// deliver a new slot, and a result taken for the old one is refused.
struct EngineSlot {
    slot: Mutex<Slot>,
    /// Shared with every [`DeliveredEngine`] an entry committed over this
    /// slot, which reads it and nothing else of the slot.
    report: Arc<Mutex<SemanticStatus>>,
    /// The configuration this slot was delivered, which an initialization
    /// opens the engine under.
    settings: Settings,
    /// Whether the slot owes a lifecycle slice: the initialization of a
    /// preparing slot, or feed a leg may have committed past the engine's
    /// positions. Read without the slot's lock, so the cheap check a
    /// maintenance clock asks never waits behind a drain.
    owed: AtomicBool,
}

impl EngineSlot {
    fn new(slot: Slot, settings: Settings) -> Arc<Self> {
        let report = Arc::new(Mutex::new(read_slot(&slot)));
        let owed = AtomicBool::new(matches!(slot, Slot::Preparing));
        Arc::new(EngineSlot {
            slot: Mutex::new(slot),
            report,
            settings,
            owed,
        })
    }

    /// The report the last act that held the slot left.
    fn report(&self) -> SemanticStatus {
        tolerant(&self.report).clone()
    }
}

/// A drain's hold of one slot, which writes the slot's report as the hold
/// ends — on every path out of the drain, an unwind included, so a panic that
/// abandoned the engine is reported as the self-disabled state every later
/// reader of the slot meets.
struct Draining<'a> {
    slot: MutexGuard<'a, Slot>,
    report: &'a Mutex<SemanticStatus>,
}

impl Drop for Draining<'_> {
    fn drop(&mut self) {
        *tolerant(self.report) = read_slot(&self.slot);
    }
}

/// The host's set of semantic engines, one delivery per attached vault and a
/// slot per vault whose delivery asked for an engine.
pub struct SemanticEngines {
    dirs: ConfigDirs,
    vaults: Mutex<BTreeMap<VaultName, Delivery>>,
    /// What a case arranged to run inside a drain's hold of its slot and as
    /// a delivery returns.
    #[cfg(test)]
    hooks: tests::Hooks,
}

/// A lock that outlives the panic that poisoned it: the state under it is
/// judged by its readers, not abandoned with the unwinding thread.
fn tolerant<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What `slot` is doing, as a status read reports it.
fn read_slot(slot: &Slot) -> SemanticStatus {
    match slot {
        Slot::Preparing => SemanticStatus::Preparing,
        Slot::Running {
            engine: Some(engine),
            last_drain_error,
        } => SemanticStatus::On {
            last_drain_error: last_drain_error.clone(),
            sidecar: engine.revision(),
            watermarks: engine.watermarks().clone(),
        },
        Slot::Running { engine: None, .. } => SemanticStatus::SelfDisabled {
            detail: ABANDONED.to_string(),
        },
        Slot::SelfDisabled { detail } => SemanticStatus::SelfDisabled {
            detail: detail.clone(),
        },
    }
}

/// A vault's engine slot as `vault status` and `doctor` report it, judged
/// against `store`: the lane-1 reading the vault's entry recorded where its
/// last leg's lane-1 work committed, where that leg owed the engine's slice.
///
/// A standing engine's freshness is [`freshness`] — the judgment a search's
/// vector rung reports against its own snapshot's reading — of its watermarks
/// against `store`: the worse of the two feeds' lags, and rescanning where a
/// watermark names another store lifetime or a feed has none. It is `None`
/// where no feed has recorded a watermark or no store reading is recorded,
/// which is the wire's own reading of an engine that has reported nothing to
/// judge. The match carries no wildcard, so a status minted without a report
/// does not compile.
pub(crate) fn engine_status(status: SemanticStatus, store: Option<&StoreReading>) -> EngineStatus {
    match status {
        SemanticStatus::Off => EngineStatus::off(),
        SemanticStatus::Preparing => EngineStatus::preparing(),
        SemanticStatus::On {
            last_drain_error,
            watermarks,
            sidecar: _,
        } => {
            let recorded = watermarks.documents.is_some() || watermarks.tombstones.is_some();
            let freshness = store
                .filter(|_| recorded)
                .map(|store| freshness(&watermarks, store));
            EngineStatus::on(last_drain_error, freshness)
        }
        SemanticStatus::SelfDisabled { detail } => EngineStatus::self_disabled(detail),
    }
}

impl SemanticEngines {
    pub fn new(dirs: ConfigDirs) -> Arc<Self> {
        Arc::new(SemanticEngines {
            dirs,
            vaults: Mutex::new(BTreeMap::new()),
            #[cfg(test)]
            hooks: tests::Hooks::default(),
        })
    }

    /// One vault's engine state, as its slot's report holds it.
    pub fn status(&self, vault: &VaultName) -> SemanticStatus {
        self.slot(vault)
            .map_or(SemanticStatus::Off, |slot| slot.report())
    }

    /// The section reading the last config delivery handed `vault`, or `None`
    /// where no delivery stands for it — the vault is not attached, or was
    /// detached since.
    pub fn section(&self, vault: &VaultName) -> Option<EngineSection> {
        tolerant(&self.vaults)
            .get(vault)
            .map(|delivery| delivery.section.clone())
    }

    /// Commit the delivery standing for `vault` as published beside `store`,
    /// and answer what it left for its entry to commit as what a status
    /// reports, or `None` where no delivery stands.
    ///
    /// **This is the publication an owed initialization waits for.** It is
    /// called in the gate hold that publishes a leg's declaration, so the
    /// section, the slot's report cell and the store lifetime the entry
    /// committed are one instant, and an initialization that follows runs for
    /// this delivery over this store lifetime alone. It takes the map lock
    /// and no slot lock, and does no I/O.
    pub(crate) fn publish_delivery(
        &self,
        vault: &VaultName,
        store: Option<&StoreReading>,
    ) -> Option<DeliveredEngine> {
        tolerant(&self.vaults).get_mut(vault).map(|delivery| {
            delivery.published_store = store.map(|store| store.epoch().to_string());
            DeliveredEngine {
                section: delivery.section.clone(),
                report: delivery.slot.as_ref().map(|slot| Arc::clone(&slot.report)),
            }
        })
    }

    /// The `limit` nearest paths to `text` in `vault` with the reading they
    /// were taken under, or the typed refusal.
    ///
    /// [`SemanticEngines::nearest_among`] over every row the engine holds.
    pub fn nearest(
        &self,
        vault: &VaultName,
        text: &str,
        limit: usize,
    ) -> Result<SemanticAnswer, SemanticRefusal> {
        self.nearest_among(vault, text, limit, |_| true)
    }

    /// The `limit` nearest paths to `text` among those `admits`, in `vault`,
    /// with the reading they were taken under, or the typed refusal.
    ///
    /// Answers whenever a slot stands: the slot is this capability's gate, and
    /// a caller composing it over a vault's store — the `search` verb, through
    /// the read seam — gates it by that vault's trust label itself. Runs on the
    /// caller's thread against the engine and its sidecar alone — never the
    /// vault's store — and serializes with the same vault's drains on the
    /// slot's lock, which is held across the rows and the reading alike. The
    /// scan holds at most `limit` scored rows ([`Engine::nearest_among`]).
    pub fn nearest_among(
        &self,
        vault: &VaultName,
        text: &str,
        limit: usize,
        admits: impl Fn(&str) -> bool,
    ) -> Result<SemanticAnswer, SemanticRefusal> {
        self.with_engine(vault, |engine| {
            let nearest = engine.nearest_among(text, limit, admits).map_err(|error| {
                SemanticRefusal::Failed {
                    detail: engine_refusal_told(&error),
                }
            })?;
            Ok(SemanticAnswer {
                neighbors: nearest.neighbors,
                model: engine.model().clone(),
                sidecar: engine.revision(),
                watermarks: engine.watermarks().clone(),
                work: nearest.work,
            })
        })
    }

    /// Whether an engine stands for `vault` here and now and, where one does,
    /// how far it had drained each feed; or the refusal a nearest answer would
    /// meet instead, read through the lookup a nearest answer takes. A reading
    /// and not a reservation: the answer that follows samples the slot again.
    pub(crate) fn standing(&self, vault: &VaultName) -> Result<Watermarks, SemanticRefusal> {
        self.with_engine(vault, |engine| Ok(engine.watermarks().clone()))
    }

    /// Run `answer` against `vault`'s running engine under the slot's lock, or
    /// answer the refusal that no engine runs.
    fn with_engine<T>(
        &self,
        vault: &VaultName,
        answer: impl FnOnce(&Engine) -> Result<T, SemanticRefusal>,
    ) -> Result<T, SemanticRefusal> {
        // The slot and, where none stands, the section the refusal carries are
        // read under one map lock, so they are one delivery's.
        let slot = match tolerant(&self.vaults).get(vault) {
            None => return Err(SemanticRefusal::NoEngine { section: None }),
            Some(Delivery {
                section,
                slot: None,
                ..
            }) => {
                return Err(SemanticRefusal::NoEngine {
                    section: Some(section.clone()),
                });
            }
            Some(Delivery {
                slot: Some(slot), ..
            }) => Arc::clone(slot),
        };
        let slot = tolerant(&slot.slot);
        match &*slot {
            Slot::Preparing => Err(SemanticRefusal::Preparing),
            Slot::Running {
                engine: Some(engine),
                ..
            } => answer(engine),
            Slot::Running { engine: None, .. } => Err(SemanticRefusal::SelfDisabled {
                detail: ABANDONED.to_string(),
            }),
            Slot::SelfDisabled { detail } => Err(SemanticRefusal::SelfDisabled {
                detail: detail.clone(),
            }),
        }
    }

    /// Record that a leg holding `vault`'s attachment may have committed
    /// lane-1 work the engine has not drained: the slot owes a slice. No
    /// semantic work runs here — the slice follows on the lifecycle's quiet
    /// maintenance, after the leg's own publication.
    pub(crate) fn owe(&self, vault: &VaultName) {
        if let Some(slot) = self.slot(vault) {
            slot.owed.store(true, Ordering::Release);
        }
    }

    /// Whether `vault`'s slot owes a slice that may run now: its delivery
    /// is published and it owes initialization or feed. Cheap and
    /// nonblocking — the map lock alone, never the slot's.
    pub(crate) fn owes_work(&self, vault: &VaultName) -> bool {
        tolerant(&self.vaults).get(vault).is_some_and(|delivery| {
            delivery.published_store.is_some()
                && delivery
                    .slot
                    .as_ref()
                    .is_some_and(|slot| slot.owed.load(Ordering::Acquire))
        })
    }

    /// Run one bounded unit of `vault`'s owed semantic work over `feed`, on a
    /// lifecycle leg that holds the vault's attachment, and answer whether
    /// work is still owed.
    ///
    /// The unit is the initialization of a preparing slot — the sidecar open
    /// or creation, its inspection or rebuild, and the model setup — or one
    /// drain slice of `budget` for a running engine. Never an error to the
    /// leg: a drain failure is retained on the slot, an open failure leaves
    /// it self-disabled, and sidecar damage resolves by the engine's own
    /// rebuild floor, whose reset cursors the next slice drains.
    ///
    /// **An initialization installs only for the delivery, store lifetime and
    /// claim it began under.** It runs only where the entry's publishing gate
    /// hold committed this slot's delivery beside `feed`'s store epoch. The
    /// open runs with no lock of this set held. The install then holds the
    /// map lock and the slot's: the map must still hold this very slot — a
    /// disable, a reload that replaced it, or a detach and re-attach under
    /// the same section and store all leave another slot or none — its
    /// published store must still be `feed`'s, and `still_claimed`, asked
    /// once the open has returned and before those locks are taken, must
    /// answer that the leg's lifecycle claim still stands. A refused result
    /// is dropped once both locks are released, so a sidecar connection is
    /// never closed under them.
    pub(crate) fn work(
        &self,
        vault: &VaultName,
        feed: &mut FeedRead<'_>,
        budget: SliceBudget,
        still_claimed: impl Fn() -> bool,
    ) -> bool {
        let store_epoch = feed.epoch().to_string();
        let Some((engine_slot, published)) =
            tolerant(&self.vaults).get(vault).and_then(|delivery| {
                delivery
                    .slot
                    .clone()
                    .map(|slot| (slot, delivery.published_store.clone()))
            })
        else {
            return false;
        };
        if !engine_slot.owed.load(Ordering::Acquire) {
            return false;
        }
        let preparing = matches!(&*tolerant(&engine_slot.slot), Slot::Preparing);
        if preparing {
            if published.as_deref() != Some(store_epoch.as_str()) {
                // Not yet committed beside this store: the publication that
                // commits it is what lets the initialization run.
                return true;
            }
            self.initialize(vault, &engine_slot, &store_epoch, still_claimed);
            return engine_slot.owed.load(Ordering::Acquire);
        }
        self.drain_slice(&engine_slot, feed, budget);
        engine_slot.owed.load(Ordering::Acquire)
    }

    /// Open the engine for `engine_slot` outside every lock, and install it
    /// where the slot is still the current, published, claimed one.
    fn initialize(
        &self,
        vault: &VaultName,
        engine_slot: &Arc<EngineSlot>,
        store_epoch: &str,
        still_claimed: impl Fn() -> bool,
    ) {
        let sidecar = self.dirs.derived_dir(vault).join(SIDECAR_FILE);
        // The slot was delivered enabling settings, and the stub is the one
        // embedder this composition opens an engine with.
        let opened = match Engine::open(&sidecar, Arc::new(StubEmbedder::new())) {
            Ok(engine) => Slot::Running {
                engine: Some(Box::new(engine)),
                last_drain_error: None,
            },
            Err(error) => Slot::SelfDisabled {
                detail: format!("the sidecar did not open: {}", engine_refusal_told(&error)),
            },
        };
        #[cfg(test)]
        self.hooks.run_before_install();
        // Asked before the map lock: the claim is read under the entry gate,
        // and the gate is taken ahead of this set's map elsewhere. A claim
        // superseded after this answer belongs to a leg whose attachment goes
        // back to a detach, which takes this slot out of the map.
        let claimed = still_claimed();
        let refused = {
            let vaults = tolerant(&self.vaults);
            let current = vaults.get(vault).is_some_and(|delivery| {
                delivery
                    .slot
                    .as_ref()
                    .is_some_and(|slot| Arc::ptr_eq(slot, engine_slot))
                    && delivery.published_store.as_deref() == Some(store_epoch)
            });
            let mut slot = tolerant(&engine_slot.slot);
            if claimed && current && matches!(&*slot, Slot::Preparing) {
                let installed = std::mem::replace(&mut *slot, opened);
                *tolerant(&engine_slot.report) = read_slot(&slot);
                // A running engine owes the drain that converges it; an
                // engine that did not open owes nothing until a delivery.
                engine_slot
                    .owed
                    .store(matches!(&*slot, Slot::Running { .. }), Ordering::Release);
                drop(slot);
                drop(vaults);
                installed
            } else {
                drop(slot);
                drop(vaults);
                opened
            }
        };
        drop(refused);
    }

    /// One drain slice of `engine_slot`'s running engine over `feed`.
    ///
    /// The slot's report is written as the slice's hold of the slot ends, on
    /// every path out of it ([`Draining`]).
    fn drain_slice(&self, engine_slot: &EngineSlot, feed: &mut FeedRead<'_>, budget: SliceBudget) {
        let mut draining = Draining {
            slot: tolerant(&engine_slot.slot),
            report: &engine_slot.report,
        };
        #[cfg(test)]
        self.hooks.run_inside_drain();
        let slot = &mut *draining.slot;
        let Slot::Running {
            engine,
            last_drain_error,
        } = &mut *slot
        else {
            engine_slot.owed.store(false, Ordering::Release);
            return;
        };
        let Some(running) = engine.as_mut() else {
            *slot = Slot::SelfDisabled {
                detail: ABANDONED.to_string(),
            };
            engine_slot.owed.store(false, Ordering::Release);
            return;
        };
        // A failed slice owes nothing more until the next leg commits: the
        // rows it did not write are still owed by the cursor, and a retry on
        // every quiet tick would only repeat the refusal.
        let owed = match running.drain_bounded(feed, budget) {
            Ok(report) => {
                *last_drain_error = None;
                report.owed
            }
            Err(error) if error.sidecar_damage().is_some() => {
                let taken = engine.take().expect("the engine this drain ran on");
                match taken.discard_and_reopen() {
                    Ok(rebuilt) => {
                        *engine = Some(Box::new(rebuilt));
                        *last_drain_error = None;
                        // The rebuilt sidecar's reset cursors are the next
                        // slice's to drain.
                        true
                    }
                    Err(failed) => {
                        *slot = Slot::SelfDisabled {
                            detail: format!(
                                "the sidecar was damaged ({}) and its rebuild failed: {}",
                                engine_refusal_told(&error),
                                engine_refusal_told(&failed)
                            ),
                        };
                        false
                    }
                }
            }
            Err(error) => {
                *last_drain_error = Some(engine_refusal_told(&error));
                false
            }
        };
        engine_slot.owed.store(owed, Ordering::Release);
        #[cfg(test)]
        self.hooks.run_as_drain_ends();
    }

    /// Give the vault's engine back with the rest of its entry's resources.
    ///
    /// Called from the lifecycle's detach, so an idled-out or deregistered
    /// vault holds no open sidecar connection and answers nothing. The
    /// sidecar file is retained state: the next delivery adopts it, cursors
    /// intact.
    ///
    /// The delivery is dropped once the map lock is released, so a sidecar
    /// connection its slot closes is never closed under that lock.
    pub(crate) fn detach(&self, vault: &VaultName) {
        let detached = tolerant(&self.vaults).remove(vault);
        drop(detached);
    }

    fn slot(&self, vault: &VaultName) -> Option<Arc<EngineSlot>> {
        tolerant(&self.vaults)
            .get(vault)
            .and_then(|delivery| delivery.slot.clone())
    }

    /// Record what a delivery left for `vault`.
    ///
    /// The delivery it replaces is dropped once the map lock is released, so
    /// a sidecar connection the replaced slot closes is never closed under
    /// that lock.
    ///
    /// A new delivery is unpublished until the entry's next publishing gate
    /// hold commits it.
    fn deliver(&self, vault: &VaultName, section: EngineSection, slot: Option<Arc<EngineSlot>>) {
        let replaced = tolerant(&self.vaults).insert(
            vault.clone(),
            Delivery {
                section,
                slot,
                published_store: None,
            },
        );
        drop(replaced);
    }
}

impl EngineConfigReceiver for SemanticEngines {
    fn name(&self) -> &str {
        "semantic"
    }

    /// Delivery validates the section and records what it asks for; it no
    /// longer opens anything.
    ///
    /// A running engine re-delivered the configuration it runs under is kept
    /// as it stands — its sidecar, cursors and diagnostics are retained
    /// state, and re-opening them would say a reload changed something it did
    /// not — and so is a slot still preparing under that configuration. An
    /// enabling section otherwise leaves a preparing slot, whose sidecar open
    /// and model setup run after the publication that commits it
    /// (`SemanticEngines::work`). A refused or self-disabled slot is
    /// re-attempted from scratch, because a delivery is exactly the author's
    /// next try.
    ///
    /// No lock of this set is held across anything but the map's own
    /// bookkeeping, and nothing here touches the filesystem.
    fn receive(&self, vault: &VaultName, config: Option<&EngineConfig>) {
        self.delivered(vault, config);
        #[cfg(test)]
        self.hooks.run_after_delivery();
    }
}

impl SemanticEngines {
    /// What [`EngineConfigReceiver::receive`] delivers: the section reading
    /// `config` is, and the slot it opens, closes or keeps.
    fn delivered(&self, vault: &VaultName, config: Option<&EngineConfig>) {
        let Some(section) = config else {
            self.deliver(vault, EngineSection::absent(), None);
            return;
        };
        let settings = match Settings::from_section(section.table()) {
            Ok(settings) => settings,
            Err(refused) => {
                let detail = refused.to_string();
                self.deliver(
                    vault,
                    EngineSection::malformed(detail.clone()),
                    Some(EngineSlot::new(
                        Slot::SelfDisabled { detail },
                        Settings { enabled: false },
                    )),
                );
                return;
            }
        };
        if !settings.enabled {
            self.deliver(vault, EngineSection::disabled(), None);
            return;
        }
        if let Some(slot) = self.slot(vault)
            && slot.settings == settings
            && matches!(
                &*tolerant(&slot.slot),
                Slot::Preparing
                    | Slot::Running {
                        engine: Some(_),
                        ..
                    }
            )
        {
            return;
        }
        self.deliver(
            vault,
            EngineSection::enabled(),
            Some(EngineSlot::new(Slot::Preparing, settings)),
        );
    }
}

#[cfg(test)]
pub(crate) mod tests {
    //! What a case arranges to run inside the engines' own critical sections.

    use std::sync::Mutex;

    use super::{SemanticEngines, tolerant};

    /// Something a case runs once, on whichever thread reaches it.
    type Arranged = Box<dyn FnOnce() + Send>;

    /// The places a case can arrange to run something inside.
    #[derive(Default)]
    pub(crate) struct Hooks {
        drain: Mutex<Option<Arranged>>,
        drain_end: Mutex<Option<Arranged>>,
        delivery: Mutex<Option<Arranged>>,
        install: Mutex<Option<Arranged>>,
    }

    impl Hooks {
        pub(super) fn run_inside_drain(&self) {
            let arranged = tolerant(&self.drain).take();
            if let Some(arranged) = arranged {
                arranged();
            }
        }

        pub(super) fn run_as_drain_ends(&self) {
            let arranged = tolerant(&self.drain_end).take();
            if let Some(arranged) = arranged {
                arranged();
            }
        }

        pub(super) fn run_before_install(&self) {
            let arranged = tolerant(&self.install).take();
            if let Some(arranged) = arranged {
                arranged();
            }
        }

        pub(super) fn run_after_delivery(&self) {
            let arranged = tolerant(&self.delivery).take();
            if let Some(arranged) = arranged {
                arranged();
            }
        }
    }

    impl SemanticEngines {
        /// Run `arranged` once, inside the next drain of any vault's engine,
        /// while that drain holds its slot and before it reads the feed.
        pub(crate) fn run_inside_next_drain(&self, arranged: impl FnOnce() + Send + 'static) {
            *tolerant(&self.hooks.drain) = Some(Box::new(arranged));
        }

        /// Run `arranged` once, as the next drain of any vault's engine that
        /// ran its engine ends: once the drain has recorded its outcome on the
        /// slot, and while it still holds the slot.
        pub(crate) fn run_as_next_drain_ends(&self, arranged: impl FnOnce() + Send + 'static) {
            *tolerant(&self.hooks.drain_end) = Some(Box::new(arranged));
        }

        /// Run `arranged` once, inside the next initialization of any vault's
        /// engine: once the engine has opened, before the install is judged,
        /// with no lock of this set held.
        pub(crate) fn run_before_next_install(&self, arranged: impl FnOnce() + Send + 'static) {
            *tolerant(&self.hooks.install) = Some(Box::new(arranged));
        }

        /// Run `arranged` once, as the next delivery to any vault returns,
        /// once what it left is recorded and no lock of this set is held.
        pub(crate) fn run_after_next_delivery(&self, arranged: impl FnOnce() + Send + 'static) {
            *tolerant(&self.hooks.delivery) = Some(Box::new(arranged));
        }
    }
}

#[cfg(test)]
mod composition_tests {
    use norn_wire::{EngineSection, ErrorDetail, ReasonCode, Rung};

    use super::{SemanticRefusal, compose_vector_refusal};

    /// Every section reading the host can hold. The match carries no wildcard,
    /// so a reading minted without a row here does not compile.
    fn every_section() -> Vec<EngineSection> {
        let sections = [
            EngineSection::undelivered(),
            EngineSection::absent(),
            EngineSection::disabled(),
            EngineSection::malformed("the `engine` table holds a string"),
            EngineSection::enabled(),
        ];
        for section in &sections {
            match section {
                EngineSection::Undelivered {}
                | EngineSection::Absent {}
                | EngineSection::Disabled {}
                | EngineSection::Malformed { .. }
                | EngineSection::Enabled {} => {}
            }
        }
        sections.to_vec()
    }

    /// Every refusal a nearest answer can be: no engine beside each section
    /// reading and beside no delivery, and the two refusals of an engine that
    /// stands. The match carries no wildcard, so a refusal minted without a
    /// row here does not compile.
    fn every_refusal() -> Vec<SemanticRefusal> {
        let mut refusals: Vec<SemanticRefusal> = every_section()
            .into_iter()
            .map(|section| SemanticRefusal::NoEngine {
                section: Some(section),
            })
            .collect();
        refusals.extend([
            SemanticRefusal::NoEngine { section: None },
            SemanticRefusal::Preparing,
            SemanticRefusal::SelfDisabled {
                detail: "the engine stood down".to_string(),
            },
            SemanticRefusal::Failed {
                detail: "the answer failed".to_string(),
            },
        ]);
        for refusal in &refusals {
            match refusal {
                SemanticRefusal::NoEngine { .. }
                | SemanticRefusal::Preparing
                | SemanticRefusal::SelfDisabled { .. }
                | SemanticRefusal::Failed { .. } => {}
            }
        }
        refusals
    }

    /// What one refusal composes to. The pairing is written out here rather
    /// than derived, so a composition that changes fails this table instead
    /// of agreeing with itself.
    fn expected(refusal: &SemanticRefusal) -> ErrorDetail {
        match refusal {
            SemanticRefusal::Failed { detail } => {
                ErrorDetail::engine_failed(Rung::Vector, detail.clone())
            }
            SemanticRefusal::SelfDisabled { detail } => {
                ErrorDetail::engine_unavailable(Rung::Vector, detail.clone())
            }
            SemanticRefusal::Preparing => {
                ErrorDetail::engine_unavailable(Rung::Vector, "the engine is preparing")
            }
            SemanticRefusal::NoEngine {
                section: Some(EngineSection::Absent {} | EngineSection::Disabled {}),
            } => ErrorDetail::engine_not_enabled(
                Rung::Vector,
                "enable the engine section in .norn/config.toml and run vault reload",
            ),
            SemanticRefusal::NoEngine {
                section: Some(EngineSection::Malformed { detail, .. }),
            } => ErrorDetail::engine_unavailable(Rung::Vector, detail.clone()),
            SemanticRefusal::NoEngine {
                section: Some(EngineSection::Enabled {}),
            } => ErrorDetail::engine_unavailable(Rung::Vector, "the engine slot is empty"),
            SemanticRefusal::NoEngine {
                section: None | Some(EngineSection::Undelivered {}),
            } => ErrorDetail::engine_unavailable(Rung::Vector, "no engine section was delivered"),
        }
    }

    /// Every refusal composes to one envelope, carrying the rung it is about
    /// and the detail pinned beside it.
    #[test]
    fn every_refusal_composes_to_one_envelope() {
        for refusal in every_refusal() {
            let envelope = compose_vector_refusal(refusal.clone());
            let detail = expected(&refusal);
            assert_eq!(
                envelope.detail(),
                &detail,
                "{refusal:?} composes to another detail"
            );
            assert_eq!(envelope.code(), &detail.code());
            assert!(
                !envelope.message().is_empty(),
                "{refusal:?} refuses without saying so in words"
            );
        }
    }

    /// A vault that never asked for an engine is told what to turn on; every
    /// other refusal is told what is wrong with the engine it asked for.
    #[test]
    fn only_a_vault_that_asked_for_no_engine_is_told_to_enable_one() {
        for refusal in every_refusal() {
            let not_enabled =
                compose_vector_refusal(refusal.clone()).code() == &ReasonCode::EngineNotEnabled;
            let asked_for_none = matches!(
                refusal,
                SemanticRefusal::NoEngine {
                    section: Some(EngineSection::Absent {} | EngineSection::Disabled {})
                }
            );
            assert_eq!(
                not_enabled, asked_for_none,
                "{refusal:?} is filed under the wrong code"
            );
        }
    }
}

#[cfg(test)]
mod freshness_tests {
    use norn_semantic::{SidecarRevision, Watermark, Watermarks};
    use norn_store::StoreReading;
    use norn_wire::{EngineStatus, Freshness};

    use super::{SemanticStatus, engine_status, freshness};

    fn at(store_epoch: &str, generation: i64) -> Option<Watermark> {
        Some(Watermark {
            store_epoch: store_epoch.to_string(),
            generation,
        })
    }

    fn marks(documents: Option<Watermark>, tombstones: Option<Watermark>) -> Watermarks {
        Watermarks {
            documents,
            tombstones,
        }
    }

    /// The answer is as stale as its stalest feed, whichever feed that is.
    #[test]
    fn freshness_is_the_worse_of_the_two_feeds() {
        let store = StoreReading::of("store", 10);
        assert_eq!(
            freshness(&marks(at("store", 7), at("store", 9)), &store),
            Freshness::trailing(3),
            "the document feed is the stalest"
        );
        assert_eq!(
            freshness(&marks(at("store", 9), at("store", 6)), &store),
            Freshness::trailing(4),
            "the tombstone feed is the stalest"
        );
        assert_eq!(
            freshness(&marks(at("store", 10), at("store", 10)), &store),
            Freshness::trailing(0)
        );
    }

    /// A feed whose watermark names another store lifetime, or none, is
    /// rescanning, and rescanning outranks any count on the other feed.
    #[test]
    fn a_feed_in_another_lifetime_or_none_reads_rescanning() {
        let store = StoreReading::of("store", 10);
        for watermarks in [
            marks(at("old", 10), at("store", 10)),
            marks(at("store", 10), at("old", 10)),
            marks(None, at("store", 10)),
            marks(at("store", 10), None),
            Watermarks::default(),
        ] {
            assert_eq!(
                freshness(&watermarks, &store),
                Freshness::rescanning(),
                "{watermarks:?}"
            );
        }
    }

    /// A watermark a later drain recorded past the reading trails it by
    /// nothing, rather than by a count that wrapped.
    #[test]
    fn a_watermark_past_the_reading_trails_by_nothing() {
        let store = StoreReading::of("store", 10);
        assert_eq!(
            freshness(&marks(at("store", 12), at("store", 11)), &store),
            Freshness::trailing(0)
        );
    }

    /// A standing engine reported with its watermarks.
    fn on(watermarks: Watermarks) -> SemanticStatus {
        SemanticStatus::On {
            last_drain_error: Some("the last drain refused".to_string()),
            sidecar: SidecarRevision {
                epoch: "sidecar".to_string(),
                revision: 3,
            },
            watermarks,
        }
    }

    /// The standing engine report carrying `freshness`.
    fn reported(freshness: Option<Freshness>) -> EngineStatus {
        EngineStatus::on(Some("the last drain refused".to_string()), freshness)
    }

    /// **A standing engine is reported with how far it trails the store
    /// reading its entry recorded**: the worse feed's lag, and rescanning
    /// where a watermark names another store lifetime or a feed has none —
    /// the judgment a search's vector rung reports against its own reading.
    #[test]
    fn a_standing_engine_reports_its_lag_behind_the_recorded_store_reading() {
        let store = StoreReading::of("store", 10);
        for (watermarks, freshness) in [
            (
                marks(at("store", 7), at("store", 9)),
                Freshness::trailing(3),
            ),
            (
                marks(at("old", 10), at("store", 10)),
                Freshness::rescanning(),
            ),
            (marks(at("store", 10), None), Freshness::rescanning()),
            (marks(None, at("store", 10)), Freshness::rescanning()),
        ] {
            assert_eq!(
                engine_status(on(watermarks.clone()), Some(&store)),
                reported(Some(freshness)),
                "{watermarks:?}"
            );
            assert_eq!(
                freshness,
                super::freshness(&watermarks, &store),
                "a status reports what a search reports"
            );
        }
    }

    /// **An engine that has recorded no watermark, or beside no store
    /// reading, reports no freshness**: there is nothing yet to judge.
    #[test]
    fn an_engine_with_no_watermark_or_no_store_reading_reports_no_freshness() {
        assert_eq!(
            engine_status(
                on(Watermarks::default()),
                Some(&StoreReading::of("store", 10))
            ),
            reported(None)
        );
        assert_eq!(
            engine_status(on(marks(at("store", 10), at("store", 10))), None),
            reported(None)
        );
    }

    /// No engine standing, an engine still preparing and an engine out of
    /// service are reported as themselves.
    #[test]
    fn an_engine_off_or_self_disabled_is_reported_as_itself() {
        let store = StoreReading::of("store", 10);
        assert_eq!(
            engine_status(SemanticStatus::Off, Some(&store)),
            EngineStatus::off()
        );
        assert_eq!(
            engine_status(SemanticStatus::Preparing, Some(&store)),
            EngineStatus::preparing()
        );
        assert_eq!(
            engine_status(
                SemanticStatus::SelfDisabled {
                    detail: "the sidecar did not open".to_string()
                },
                Some(&store)
            ),
            EngineStatus::self_disabled("the sidecar did not open")
        );
    }
}
