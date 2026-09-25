//! The semantic engines the host composes: one per enabled vault.
//!
//! This is the crate-map edge `norn-host → norn-semantic`, and the whole of
//! it. The host owns three things here and defines nothing past them:
//!
//! - **Delivery is the enable act.** The config dispatch hands this set the
//!   vault's `[engine.semantic]` section; a section that parses opens the
//!   engine eagerly at that delivery, an absent section (or `enabled =
//!   false`) closes it, and a section the engine refuses — or an engine that
//!   cannot open — leaves a typed self-disabled state a status read reports.
//!   What the section *means* is the engine's own ([`norn_semantic::Settings`]).
//!   The reading of the section itself — absent, disabled, malformed with the
//!   engine's refusal, or enabled — is retained beside the slot at the same
//!   delivery ([`SemanticEngines::section`]), and a refusal that no engine
//!   stands carries the reading taken under the same lookup as the slot it
//!   found missing, because "no engine stands" is answered differently for a
//!   vault that asked for none and one whose section could not be read.
//! - **The nudge is a post-leg pull.** Every leg that ends holding a
//!   consistent lane-1 store relays the vault's feed handle here and the
//!   engine drains on that same worker leg — the increments, every heal
//!   (attach, recover, schema reload, and the store rebuild whose new epoch
//!   is exactly what a cursor must not sleep through), and the config-only
//!   reload, whose drain is what converges a corpus derived before its
//!   engine existed. This is the nudge transport's floor: no thread, no
//!   channel, no missable wake — a dedicated engine worker is the carve
//!   drain cost forces, not this composition's obligation. A drain failure
//!   never fails the leg: lane 2's contract is eventual consistency, so
//!   engine trouble is retained as the slot's own diagnostic, and sidecar
//!   damage resolves by the engine's rebuild floor right here.
//! - **Answers come from the slot, gated by the slot alone.** A nearest
//!   answer needs the engine and its sidecar, never the vault's store, so it
//!   runs on the caller's thread and answers whenever a slot stands — the
//!   vault's trust label does not gate the capability. A status reads the
//!   report kept beside the slot rather than the slot itself, through the
//!   delivery the vault's entry committed with the fingerprints it publishes
//!   ([`DeliveredEngine`]), so it never reads this set. The `search` verb
//!   composes it through the read seam, so a search's vector rung answers only
//!   for a ready entry, beside the snapshot its hold established.
//! - **An answer carries its own reading.** A nearest answer is sampled under
//!   the slot's lock together with the model it ran, the sidecar revision it
//!   was taken from and the engine's watermarks, so no drain lands between
//!   the rows and the reading that describes them. Freshness is those
//!   watermarks judged against the store reading the caller's own hold
//!   established ([`freshness`]).
//!
//! # Locking, and the guarantee it leans on
//!
//! The outer map lock covers lookup, delivery bookkeeping and teardown only
//! — never an engine open, a drain, or an answer — so one vault's work never
//! holds another vault's. Within one vault, a slot's own lock serializes its
//! drains and its answers against each other, and a drain holds it for the
//! whole drain. The report beside each slot has a lock of its own, written
//! while the slot's lock is held and read without it, so a status taken
//! during a drain reads the slot as it stood before that drain rather than
//! waiting for it to end ([`EngineSlot`]). Slot replacement at delivery
//! is sound because the lifecycle admits one leg per vault at a time (the
//! entry's custody claim); a lifecycle that relaxed that would need delivery
//! to mutate in place rather than replace.
//!
//! Every slot lock tolerates poison: a panic that escaped the engine never
//! becomes a second panic on a worker leg or a caller's thread. What it leaves
//! depends on where it escaped — an engine borrowed for an ordinary drain or a
//! nearest answer stays in the slot, and an engine taken out for the
//! sidecar-damage rebuild is abandoned, which every later reader answers as
//! the self-disabled state.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use norn_config::ConfigDirs;
use norn_config::vault::EngineConfig;
use norn_embed::{Model, StubEmbedder};
use norn_semantic::{
    Engine, NearestWork, Neighbor, Settings, SidecarRevision, Watermark, Watermarks,
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
/// [`VectorRefusal::skip_reason`], and is refused by a failed one. The entry is
/// ready wherever this is reached: the read seam refuses a vault that is not
/// with the ordinary answer-reading refusal before any rung runs.
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
struct EngineSlot {
    slot: Mutex<Slot>,
    /// Shared with every [`DeliveredEngine`] an entry committed over this
    /// slot, which reads it and nothing else of the slot.
    report: Arc<Mutex<SemanticStatus>>,
}

impl EngineSlot {
    fn new(slot: Slot) -> Arc<Self> {
        let report = Arc::new(Mutex::new(read_slot(&slot)));
        Arc::new(EngineSlot {
            slot: Mutex::new(slot),
            report,
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
/// last leg's lane-1 work committed, ahead of the drain that leg relays.
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

    /// What the delivery standing for `vault` left, for its entry to commit
    /// as what a status reports, or `None` where no delivery stands.
    ///
    /// The section and the slot's report cell come from one delivery: the
    /// map lock is held across finding them.
    pub(crate) fn delivery(&self, vault: &VaultName) -> Option<DeliveredEngine> {
        tolerant(&self.vaults)
            .get(vault)
            .map(|delivery| DeliveredEngine {
                section: delivery.section.clone(),
                report: delivery.slot.as_ref().map(|slot| Arc::clone(&slot.report)),
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

    /// Drain `vault`'s engine over `feed`, on the leg that just committed
    /// lane-1 work.
    ///
    /// Never an error to the leg: a drain failure is retained on the slot,
    /// and sidecar damage resolves by the engine's own rebuild floor — the
    /// rebuilt sidecar's reset cursors make the next drain recompute what
    /// the discard emptied.
    ///
    /// The slot's report is written as the drain's hold of the slot ends, on
    /// every path out of it ([`Draining`]).
    pub(crate) fn drain(&self, vault: &VaultName, feed: &mut FeedRead<'_>) {
        let Some(engine_slot) = self.slot(vault) else {
            return;
        };
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
            return;
        };
        let Some(running) = engine.as_mut() else {
            *slot = Slot::SelfDisabled {
                detail: ABANDONED.to_string(),
            };
            return;
        };
        match running.drain(feed) {
            Ok(_) => *last_drain_error = None,
            Err(error) if error.sidecar_damage().is_some() => {
                let taken = engine.take().expect("the engine this drain ran on");
                match taken.discard_and_reopen() {
                    Ok(mut rebuilt) => {
                        let outcome = rebuilt.drain(feed);
                        *engine = Some(Box::new(rebuilt));
                        *last_drain_error = match outcome {
                            Ok(_) => None,
                            Err(after) => Some(format!(
                                "rebuilt after damage ({}), and the drain after it failed: {}",
                                engine_refusal_told(&error),
                                engine_refusal_told(&after)
                            )),
                        };
                    }
                    Err(failed) => {
                        *slot = Slot::SelfDisabled {
                            detail: format!(
                                "the sidecar was damaged ({}) and its rebuild failed: {}",
                                engine_refusal_told(&error),
                                engine_refusal_told(&failed)
                            ),
                        };
                    }
                }
            }
            Err(error) => *last_drain_error = Some(engine_refusal_told(&error)),
        }
        #[cfg(test)]
        self.hooks.run_as_drain_ends();
    }

    /// Give the vault's engine back with the rest of its entry's resources.
    ///
    /// Called from the lifecycle's detach, so an idled-out or deregistered
    /// vault holds no open sidecar connection and answers nothing. The
    /// sidecar file is retained state: the next delivery adopts it, cursors
    /// intact.
    pub(crate) fn detach(&self, vault: &VaultName) {
        tolerant(&self.vaults).remove(vault);
    }

    fn slot(&self, vault: &VaultName) -> Option<Arc<EngineSlot>> {
        tolerant(&self.vaults)
            .get(vault)
            .and_then(|delivery| delivery.slot.clone())
    }

    /// Record what a delivery left for `vault`.
    fn deliver(&self, vault: &VaultName, section: EngineSection, slot: Option<Slot>) {
        tolerant(&self.vaults).insert(
            vault.clone(),
            Delivery {
                section,
                slot: slot.map(EngineSlot::new),
            },
        );
    }
}

impl EngineConfigReceiver for SemanticEngines {
    fn name(&self) -> &str {
        "semantic"
    }

    /// Delivery is the enable act.
    ///
    /// A running engine re-delivered an enabling section is kept as it
    /// stands — its sidecar, cursors and diagnostics are retained state, and
    /// re-opening them would say a reload changed something it did not.
    /// Keep-versus-reopen has no observable difference today (the sidecar is
    /// adopted either way), so the choice is stated here rather than tested.
    /// A refused or self-disabled slot is re-attempted from scratch, because
    /// a delivery is exactly the author's next try.
    ///
    /// The map lock is never held across the engine open: the open touches
    /// the filesystem, and holding the map through it would couple every
    /// vault's answers to this one's enable.
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
                    Some(Slot::SelfDisabled { detail }),
                );
                return;
            }
        };
        if !settings.enabled {
            self.deliver(vault, EngineSection::disabled(), None);
            return;
        }
        if let Some(slot) = self.slot(vault)
            && matches!(
                &*tolerant(&slot.slot),
                Slot::Running {
                    engine: Some(_),
                    ..
                }
            )
        {
            return;
        }
        let sidecar = self.dirs.derived_dir(vault).join(SIDECAR_FILE);
        let slot = match Engine::open(&sidecar, Arc::new(StubEmbedder::new())) {
            Ok(engine) => Slot::Running {
                engine: Some(Box::new(engine)),
                last_drain_error: None,
            },
            Err(error) => Slot::SelfDisabled {
                detail: format!("the sidecar did not open: {}", engine_refusal_told(&error)),
            },
        };
        self.deliver(vault, EngineSection::enabled(), Some(slot));
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

    /// No engine standing and an engine out of service are reported as
    /// themselves.
    #[test]
    fn an_engine_off_or_self_disabled_is_reported_as_itself() {
        let store = StoreReading::of("store", 10);
        assert_eq!(
            engine_status(SemanticStatus::Off, Some(&store)),
            EngineStatus::off()
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
