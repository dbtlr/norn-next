#![forbid(unsafe_code)]
//! Protocol-blind orchestration for registered vaults.
//!
//! [`Host`] owns registry semantics and the lifecycle of each vault entry. It
//! deliberately delegates vault and store effects through [`EntryOps`]: the
//! host decides *when* attach, reconciliation and detach happen, while the
//! filesystem and store crates remain the only owners of those effects.

mod address;
mod derivation;
mod evidence;
mod lifecycle;
mod production;
mod read;
mod refusal;
mod registry;
mod reload;
mod semantic;

pub use derivation::DERIVATION_VERSION;
/// **The harness-reachable readers of a host's own account.** Every job writes
/// the account whatever features are on; reading it is what this feature opens,
/// beside [`ProductionEntryOps::account`], [`Host::classifications`] and
/// [`Host::recovery_demands`].
#[cfg(feature = "induced-failure")]
pub use evidence::{EvidenceReading, JobEvidence};
pub use evidence::{ReadReading, ReadsSince};
/// **What is running against an entry**, read by the harness that has to know
/// the host stopped working before it measures one at rest. Behind the same
/// feature as the rest of [`Host`]'s harness-reachable readers.
#[cfg(feature = "induced-failure")]
pub use lifecycle::WorkInFlight;
pub use lifecycle::{
    Demand, DemandLease, EntryOps, EntryReloadFailure, Established, Establishment, Healing,
    HoldReading, Host, HostError, JobFailure, LifecyclePolicy, LifecyclePolicyError, MintedReader,
    ProgressReporter, ReadHold, ReadRefusal, ReadSource, ReaderUnavailable, ReconcileWork,
    SnapshotSource,
};
/// One vault's registration: the name it is served under, its root, and where
/// its schema is read from.
///
/// [`EntryOps::attach`] is handed one, so an implementation of that trait names
/// the type through this crate rather than reaching for the config crate's
/// spelling of it.
pub use norn_config::registry::Entry as Registration;
/// How the derived state a demand asks for is held.
///
/// Registration is what gates durability, so the mode is the demand's own
/// rather than a property read off the entry: a registered vault's derivation
/// is durable, and disposable derivation over a throwaway store is the other
/// mode the same seam carries.
///
/// **The throwaway mode is a dormant carrier.** The layer that consumes it is
/// disposable derivation for unregistered roots, which `docs/architecture.md`
/// names in its topology section: a root nobody registered is served, when that
/// layer lands, by deriving over a throwaway store and throwing it away with
/// the work. The store-side half of that seam is built — `norn-store` opens a
/// throwaway store today — while nothing here establishes an entry over one,
/// which is why the only paths this crate's call graph reaches the mode by are
/// refusals: a demand naming it, and a request addressing its vault by root.
pub use norn_wire::AttachMode;
pub use production::{
    MAX_CHANGESET_SIZE, ProductionEntryOps, ProductionPolicy, ProductionPolicyError,
    WATCH_SYNCHRONIZATION_DEADLINE, stored_path_order,
};
pub use read::Answered;
pub use registry::{AliasConflict, RegistryRead};
pub use reload::{
    ActiveFingerprints, AuthoredDrift, ConfigFingerprint, EngineConfigReceiver, ReloadError,
    ReloadFile, ReloadOutcome, ReloadRefusal, ReloadStage, VaultInspection,
};
pub use semantic::{
    SemanticAnswer, SemanticEngines, SemanticRefusal, SemanticStatus, compose_vector_refusal,
    freshness,
};
