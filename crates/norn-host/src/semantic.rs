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
//! - **The nudge is a post-leg pull.** After a lifecycle leg commits lane-1
//!   work, the ops relay the vault's feed handle here and the engine drains
//!   on the same worker leg. This is the nudge transport's floor: no thread,
//!   no channel, no missable wake — a dedicated engine worker is the carve
//!   drain cost forces, not this composition's obligation. A drain failure
//!   never fails the leg: lane 2's contract is eventual consistency, so
//!   engine trouble is retained as the slot's own diagnostic, and sidecar
//!   damage resolves by the engine's rebuild floor right here.
//! - **Vector-nearest is answered from the slot.** A query needs the engine
//!   and its sidecar, never the vault's store, so it runs on the caller's
//!   thread against the slot alone.
//!
//! Locking is per vault: the outer map lock covers lookup and delivery, each
//! slot's own lock covers a drain or an answer, so one vault's drain never
//! holds another vault's answers.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

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
        /// `Some` only across the engine's own rebuild inside a drain; every
        /// lock release leaves it occupied.
        engine: Option<Engine>,
        /// The most recent drain's failure, cleared by the next drain that
        /// succeeds. Latency, never correctness: the rows the failed drain
        /// did not write are still owed by the cursor, and the next nudge
        /// re-attempts them.
        last_drain_error: Option<String>,
    },
    /// The engine took itself out of service: its section was refused, its
    /// sidecar could not open, or a rebuild after damage failed. The next
    /// config delivery is what re-attempts it.
    SelfDisabled { detail: String },
}

/// What a status read reports for one vault.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticStatus {
    /// No section stands for this vault, so no engine does.
    Off,
    On {
        last_drain_error: Option<String>,
    },
    SelfDisabled {
        detail: String,
    },
}

/// Why a nearest answer was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticRefusal {
    /// No section enables the engine for this vault.
    Disabled,
    /// The engine took itself out of service; the detail is the slot's.
    SelfDisabled { detail: String },
    /// The engine is running and this answer failed.
    Failed { detail: String },
}

impl std::fmt::Display for SemanticRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SemanticRefusal::Disabled => {
                write!(
                    f,
                    "no engine section enables semantic search for this vault"
                )
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
        match &*slot.lock().expect("a semantic slot") {
            Slot::Running {
                last_drain_error, ..
            } => SemanticStatus::On {
                last_drain_error: last_drain_error.clone(),
            },
            Slot::SelfDisabled { detail } => SemanticStatus::SelfDisabled {
                detail: detail.clone(),
            },
        }
    }

    /// The `limit` nearest paths to `text` in `vault`, or the typed refusal.
    ///
    /// Runs on the caller's thread against the slot alone: the answer needs
    /// the engine and its sidecar, never the vault's store.
    pub fn nearest(
        &self,
        vault: &VaultName,
        text: &str,
        limit: usize,
    ) -> Result<Vec<Neighbor>, SemanticRefusal> {
        let Some(slot) = self.slot(vault) else {
            return Err(SemanticRefusal::Disabled);
        };
        let slot = slot.lock().expect("a semantic slot");
        match &*slot {
            Slot::Running { engine, .. } => engine
                .as_ref()
                .expect("a released slot holds its engine")
                .nearest(text, limit)
                .map_err(|error| SemanticRefusal::Failed {
                    detail: error.to_string(),
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
        let mut slot = slot.lock().expect("a semantic slot");
        let Slot::Running {
            engine,
            last_drain_error,
        } = &mut *slot
        else {
            return;
        };
        let running = engine.as_mut().expect("a released slot holds its engine");
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

    fn slot(&self, vault: &VaultName) -> Option<Arc<Mutex<Slot>>> {
        self.vaults
            .lock()
            .expect("the semantic vault map")
            .get(vault)
            .cloned()
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
    /// re-opening them would say a reload changed something it did not. A
    /// refused or self-disabled slot is re-attempted from scratch, because a
    /// delivery is exactly the author's next try.
    fn receive(&self, vault: &VaultName, config: Option<&EngineConfig>) {
        let mut vaults = self.vaults.lock().expect("the semantic vault map");
        let Some(section) = config else {
            vaults.remove(vault);
            return;
        };
        let settings = match Settings::from_section(section.table()) {
            Ok(settings) => settings,
            Err(refused) => {
                vaults.insert(
                    vault.clone(),
                    Arc::new(Mutex::new(Slot::SelfDisabled {
                        detail: refused.to_string(),
                    })),
                );
                return;
            }
        };
        if !settings.enabled {
            vaults.remove(vault);
            return;
        }
        if let Some(slot) = vaults.get(vault)
            && matches!(
                &*slot.lock().expect("a semantic slot"),
                Slot::Running { .. }
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
        vaults.insert(vault.clone(), Arc::new(Mutex::new(slot)));
    }
}
