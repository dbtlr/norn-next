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
//! Every slot lock tolerates poison: a panic that escaped the engine ends as
//! the slot's own self-disabled state, never as a second panic on a worker
//! leg or a caller's thread.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use norn_config::ConfigDirs;
use norn_config::vault::EngineConfig;
use norn_embed::StubEmbedder;
use norn_semantic::{Engine, Neighbor, Settings};
use norn_store::FeedRead;
use norn_wire::VaultName;

use crate::reload::EngineConfigReceiver;

/// The sidecar's file, beside the vault's `store.sqlite3` in the same
/// derived directory.
const SIDECAR_FILE: &str = "semantic.sqlite3";

/// One vault's engine, or the typed reason it is not running.
enum Slot {
    Running {
        /// `None` only when a panic escaped mid-operation and abandoned the
        /// engine; every reader maps that reading to a self-disabled answer.
        engine: Option<Engine>,
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
    },
    SelfDisabled {
        detail: String,
    },
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

/// The host's set of semantic engines, one slot per enabled vault.
pub struct SemanticEngines {
    dirs: ConfigDirs,
    vaults: Mutex<BTreeMap<VaultName, Arc<Mutex<Slot>>>>,
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
                engine: Some(_),
                last_drain_error,
            } => SemanticStatus::On {
                last_drain_error: last_drain_error.clone(),
            },
            Slot::Running { engine: None, .. } => SemanticStatus::SelfDisabled {
                detail: ABANDONED.to_string(),
            },
            Slot::SelfDisabled { detail } => SemanticStatus::SelfDisabled {
                detail: detail.clone(),
            },
        }
    }

    /// The `limit` nearest paths to `text` in `vault`, or the typed refusal.
    ///
    /// Answers whenever a slot stands: the slot, not the vault's trust
    /// label, is the gate. Runs on the caller's thread against the engine
    /// and its sidecar alone — never the vault's store — and serializes with
    /// the same vault's drains on the slot's lock.
    pub fn nearest(
        &self,
        vault: &VaultName,
        text: &str,
        limit: usize,
    ) -> Result<Vec<Neighbor>, SemanticRefusal> {
        let Some(slot) = self.slot(vault) else {
            return Err(SemanticRefusal::NoEngine);
        };
        let slot = tolerant(&slot);
        match &*slot {
            Slot::Running {
                engine: Some(engine),
                ..
            } => engine
                .nearest(text, limit)
                .map_err(|error| SemanticRefusal::Failed {
                    detail: error.to_string(),
                }),
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
                        *engine = Some(rebuilt);
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
        tolerant(&self.vaults).get(vault).cloned()
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
            tolerant(&self.vaults).remove(vault);
            return;
        };
        let settings = match Settings::from_section(section.table()) {
            Ok(settings) => settings,
            Err(refused) => {
                tolerant(&self.vaults).insert(
                    vault.clone(),
                    Arc::new(Mutex::new(Slot::SelfDisabled {
                        detail: refused.to_string(),
                    })),
                );
                return;
            }
        };
        if !settings.enabled {
            tolerant(&self.vaults).remove(vault);
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
                engine: Some(engine),
                last_drain_error: None,
            },
            Err(error) => Slot::SelfDisabled {
                detail: format!("the sidecar did not open: {error}"),
            },
        };
        tolerant(&self.vaults).insert(vault.clone(), Arc::new(Mutex::new(slot)));
    }
}
