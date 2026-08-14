#![forbid(unsafe_code)]
//! Protocol-blind orchestration for registered vaults.
//!
//! [`Host`] owns registry semantics and the lifecycle of each vault entry. It
//! deliberately delegates vault and store effects through [`EntryOps`]: the
//! host decides *when* attach, reconciliation and detach happen, while the
//! filesystem and store crates remain the only owners of those effects.

mod lifecycle;
mod production;
mod refusal;
mod registry;

pub use lifecycle::{
    Demand, DemandLease, EntryOps, Healing, Host, HostError, JobFailure, LifecyclePolicy,
    ProgressReporter, ReadHold, ReconcileWork, SnapshotSource,
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
/// which is why the only path this crate's call graph reaches the mode by is
/// the refusal at the demand seam.
pub use norn_wire::AttachMode;
pub use production::{
    MAX_CHANGESET_SIZE, ProductionEntryOps, ProductionPolicy, ProductionPolicyError,
};
pub use registry::{AliasConflict, RegistryRead};

// `norn-embed` is a declared architecture edge that no module consumes yet.
// The witness keeps the dependency allowlist and manifest in agreement.
#[allow(unused_imports)]
use norn_embed as _;
