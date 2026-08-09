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
    AttachMode, Demand, DemandLease, EntryOps, Healing, Host, HostError, JobFailure,
    LifecyclePolicy, ProgressReporter, ReadHold, ReconcileWork, SnapshotSource,
};
/// One vault's registration: the name it is served under, its root, and where
/// its schema is read from.
///
/// [`EntryOps::attach`] is handed one, so an implementation of that trait names
/// the type through this crate rather than reaching for the config crate's
/// spelling of it.
pub use norn_config::registry::Entry as Registration;
pub use production::{
    MAX_CHANGESET_SIZE, ProductionEntryOps, ProductionPolicy, ProductionPolicyError,
};
pub use registry::{AliasConflict, RegistryRead};

// `norn-embed` is a declared architecture edge that no module consumes yet.
// The witness keeps the dependency allowlist and manifest in agreement.
#[allow(unused_imports)]
use norn_embed as _;
