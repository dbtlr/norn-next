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
//!   delivery ([`SemanticEngines::section`]), because "no engine stands" is
//!   answered differently for a vault that asked for none and one whose
//!   section could not be read.
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
//! - **Answers come from the slot, gated by the slot alone.** A nearest or
//!   status read needs the engine and its sidecar, never the vault's store,
//!   so it runs on the caller's thread and answers whenever a slot stands —
//!   the vault's trust label does not gate it. Composing trust over semantic
//!   answers is the serving surface's judgment, made where that surface is
//!   built.
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
//! drains and its answers against each other. Slot replacement at delivery
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
use norn_semantic::{Engine, Neighbor, Settings, SidecarRevision, Watermark, Watermarks};
use norn_store::{FeedRead, StoreReading};
use norn_wire::{EngineSection, ErrorDetail, ErrorEnvelope, Freshness, Rung, VaultName};

use crate::reload::EngineConfigReceiver;

/// The sidecar's file, beside the vault's `store.sqlite3` in the same
/// derived directory.
const SIDECAR_FILE: &str = "semantic.sqlite3";

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
/// Nothing calls it yet: the `search` and `status` handlers are its callers,
/// and they compose it with the hold reading each answer is taken under.
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
    /// No engine stands for this vault: no delivered section enables one,
    /// or the vault is not attached. Which of the two is the serving
    /// surface's to say — it composes the config and entry state this
    /// slot-gated reading deliberately does not.
    NoEngine,
    /// The engine took itself out of service; the detail is the slot's.
    SelfDisabled { detail: String },
    /// The engine is running and this answer failed.
    Failed { detail: String },
}

impl std::fmt::Display for SemanticRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SemanticRefusal::NoEngine => {
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

/// The refusal a vector rung answers with, composed from what the engine said
/// and what the host was delivered as the vault's engine section.
///
/// The engine's own reading is slot-gated and deliberately says only that no
/// engine stands; which of "the vault never enabled one" and "the vault's
/// section could not be read" is true is a fact about the delivered section,
/// which the engine does not hold. Composing the two here is what lets a
/// client be told what to do about it.
///
/// This is reached only where the entry is ready; a vault that is not ready
/// refuses on its answer reading long before a rung is dispatched.
///
/// Nothing calls it yet. The `search` handler is the one caller this
/// composition has: it pairs the refusal [`SemanticEngines::nearest`] answered
/// with the section [`SemanticEngines::section`] retained at the same
/// delivery, and no call graph reaches this until that handler arrives.
///
/// Both matches carry no wildcard, so a refusal or a section minted without a
/// row here does not compile.
pub fn compose_vector_refusal(section: &EngineSection, refusal: SemanticRefusal) -> ErrorEnvelope {
    match refusal {
        SemanticRefusal::NoEngine => match section {
            // The vault has not asked for an engine, so there is nothing
            // unavailable — there is something to turn on.
            EngineSection::Absent {} | EngineSection::Disabled {} => ErrorEnvelope::new(
                "this vault answers no vector rung until its engine is enabled",
                ErrorDetail::engine_not_enabled(
                    Rung::Vector,
                    "enable the engine section in .norn/config.toml and run vault reload",
                ),
            ),
            // The vault asked for an engine and the section it asked with
            // could not be read, so the engine was never delivered.
            EngineSection::Malformed { detail, .. } => ErrorEnvelope::new(
                "this vault's engine section could not be read, so no engine stands for it",
                ErrorDetail::engine_unavailable(Rung::Vector, detail.clone()),
            ),
            // An enabled section with no engine behind it is a slot the
            // delivery should have filled. Dispatch runs only against a ready
            // entry, whose delivery has already happened, so this row is not
            // reached by that ordering; it is answered rather than panicked on,
            // because a wire mapping is not the place a host asserts its own
            // invariants.
            EngineSection::Enabled {} => ErrorEnvelope::new(
                "this vault's engine is enabled and no engine stands for it",
                ErrorDetail::engine_unavailable(Rung::Vector, "the engine slot is empty"),
            ),
        },
        // The engine took itself out of service. What the section says is not
        // the fact any more: the engine was delivered and stood down.
        SemanticRefusal::SelfDisabled { detail } => ErrorEnvelope::new(
            "this vault's engine is out of service, so it answers no vector rung",
            ErrorDetail::engine_unavailable(Rung::Vector, detail),
        ),
        // The engine stands and this answer failed.
        SemanticRefusal::Failed { detail } => ErrorEnvelope::new(
            "this vault's engine failed to answer the vector rung",
            ErrorDetail::engine_failed(Rung::Vector, detail),
        ),
    }
}

/// What the last config delivery left for one vault: the section reading it
/// was delivered, and the slot that reading opened, where it opened one.
///
/// One entry under one lock, so a reader looking both up sees a pair one
/// delivery produced.
struct Delivery {
    section: EngineSection,
    slot: Option<Arc<Mutex<Slot>>>,
}

/// The host's set of semantic engines, one delivery per attached vault and a
/// slot per vault whose delivery asked for an engine.
pub struct SemanticEngines {
    dirs: ConfigDirs,
    vaults: Mutex<BTreeMap<VaultName, Delivery>>,
}

/// A lock that outlives the panic that poisoned it: the state under it is
/// judged by its readers, not abandoned with the unwinding thread.
fn tolerant<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(PoisonError::into_inner)
}

impl SemanticEngines {
    pub fn new(dirs: ConfigDirs) -> Arc<Self> {
        Arc::new(SemanticEngines {
            dirs,
            vaults: Mutex::new(BTreeMap::new()),
        })
    }

    /// One vault's engine state.
    pub fn status(&self, vault: &VaultName) -> SemanticStatus {
        let Some(slot) = self.slot(vault) else {
            return SemanticStatus::Off;
        };
        match &*tolerant(&slot) {
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

    /// The section reading the last config delivery handed `vault`, or `None`
    /// where no delivery stands for it — the vault is not attached, or was
    /// detached since.
    pub fn section(&self, vault: &VaultName) -> Option<EngineSection> {
        tolerant(&self.vaults)
            .get(vault)
            .map(|delivery| delivery.section.clone())
    }

    /// The `limit` nearest paths to `text` in `vault` with the reading they
    /// were taken under, or the typed refusal.
    ///
    /// Answers whenever a slot stands: the slot, not the vault's trust
    /// label, is the gate. Runs on the caller's thread against the engine
    /// and its sidecar alone — never the vault's store — and serializes with
    /// the same vault's drains on the slot's lock, which is held across the
    /// rows and the reading alike.
    pub fn nearest(
        &self,
        vault: &VaultName,
        text: &str,
        limit: usize,
    ) -> Result<SemanticAnswer, SemanticRefusal> {
        let Some(slot) = self.slot(vault) else {
            return Err(SemanticRefusal::NoEngine);
        };
        let slot = tolerant(&slot);
        match &*slot {
            Slot::Running {
                engine: Some(engine),
                ..
            } => {
                let neighbors =
                    engine
                        .nearest(text, limit)
                        .map_err(|error| SemanticRefusal::Failed {
                            detail: error.to_string(),
                        })?;
                Ok(SemanticAnswer {
                    neighbors,
                    model: engine.model().clone(),
                    sidecar: engine.revision(),
                    watermarks: engine.watermarks().clone(),
                })
            }
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
    pub(crate) fn drain(&self, vault: &VaultName, feed: &mut FeedRead<'_>) {
        let Some(slot) = self.slot(vault) else {
            return;
        };
        let mut slot = tolerant(&slot);
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
                                "rebuilt after damage ({error}), and the drain after it \
                                 failed: {after}"
                            )),
                        };
                    }
                    Err(failed) => {
                        *slot = Slot::SelfDisabled {
                            detail: format!(
                                "the sidecar was damaged ({error}) and its rebuild failed: \
                                 {failed}"
                            ),
                        };
                    }
                }
            }
            Err(error) => *last_drain_error = Some(error.to_string()),
        }
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

    fn slot(&self, vault: &VaultName) -> Option<Arc<Mutex<Slot>>> {
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
                slot: slot.map(|slot| Arc::new(Mutex::new(slot))),
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
                &*tolerant(&slot),
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
                detail: format!("the sidecar did not open: {error}"),
            },
        };
        self.deliver(vault, EngineSection::enabled(), Some(slot));
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
            EngineSection::absent(),
            EngineSection::disabled(),
            EngineSection::malformed("the `engine` table holds a string"),
            EngineSection::enabled(),
        ];
        for section in &sections {
            match section {
                EngineSection::Absent {}
                | EngineSection::Disabled {}
                | EngineSection::Malformed { .. }
                | EngineSection::Enabled {} => {}
            }
        }
        sections.to_vec()
    }

    /// Every refusal the engine's own reading can be. The match carries no
    /// wildcard, so a refusal minted without a row here does not compile.
    fn every_refusal() -> Vec<SemanticRefusal> {
        let refusals = vec![
            SemanticRefusal::NoEngine,
            SemanticRefusal::SelfDisabled {
                detail: "the engine stood down".to_string(),
            },
            SemanticRefusal::Failed {
                detail: "the answer failed".to_string(),
            },
        ];
        for refusal in &refusals {
            match refusal {
                SemanticRefusal::NoEngine
                | SemanticRefusal::SelfDisabled { .. }
                | SemanticRefusal::Failed { .. } => {}
            }
        }
        refusals
    }

    /// What one pair composes to. The pairing is written out here rather than
    /// derived, so a composition that changes fails this table instead of
    /// agreeing with itself.
    fn expected(section: &EngineSection, refusal: &SemanticRefusal) -> ErrorDetail {
        match (section, refusal) {
            (_, SemanticRefusal::Failed { detail }) => {
                ErrorDetail::engine_failed(Rung::Vector, detail.clone())
            }
            (_, SemanticRefusal::SelfDisabled { detail }) => {
                ErrorDetail::engine_unavailable(Rung::Vector, detail.clone())
            }
            (EngineSection::Absent {} | EngineSection::Disabled {}, SemanticRefusal::NoEngine) => {
                ErrorDetail::engine_not_enabled(
                    Rung::Vector,
                    "enable the engine section in .norn/config.toml and run vault reload",
                )
            }
            (EngineSection::Malformed { detail, .. }, SemanticRefusal::NoEngine) => {
                ErrorDetail::engine_unavailable(Rung::Vector, detail.clone())
            }
            (EngineSection::Enabled {}, SemanticRefusal::NoEngine) => {
                ErrorDetail::engine_unavailable(Rung::Vector, "the engine slot is empty")
            }
        }
    }

    /// Every pair of a delivered section and an engine refusal composes to one
    /// envelope, carrying the rung it is about and the detail pinned beside
    /// it.
    #[test]
    fn every_section_and_refusal_pair_composes_to_one_envelope() {
        for section in every_section() {
            for refusal in every_refusal() {
                let envelope = compose_vector_refusal(&section, refusal.clone());
                let detail = expected(&section, &refusal);
                assert_eq!(
                    envelope.detail(),
                    &detail,
                    "{section:?} with {refusal:?} composes to another detail"
                );
                assert_eq!(envelope.code(), &detail.code());
                assert!(
                    !envelope.message().is_empty(),
                    "{section:?} with {refusal:?} refuses without saying so in words"
                );
            }
        }
    }

    /// A vault that never asked for an engine is told what to turn on; every
    /// other pair is told what is wrong with the engine it asked for.
    #[test]
    fn only_a_vault_that_asked_for_no_engine_is_told_to_enable_one() {
        for section in every_section() {
            for refusal in every_refusal() {
                let not_enabled = compose_vector_refusal(&section, refusal.clone()).code()
                    == &ReasonCode::EngineNotEnabled;
                let asked_for_none = matches!(
                    section,
                    EngineSection::Absent {} | EngineSection::Disabled {}
                ) && refusal == SemanticRefusal::NoEngine;
                assert_eq!(
                    not_enabled, asked_for_none,
                    "{section:?} with {refusal:?} is filed under the wrong code"
                );
            }
        }
    }
}

#[cfg(test)]
mod freshness_tests {
    use norn_semantic::{Watermark, Watermarks};
    use norn_store::StoreReading;
    use norn_wire::Freshness;

    use super::freshness;

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
}
