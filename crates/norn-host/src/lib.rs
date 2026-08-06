#![forbid(unsafe_code)]
//! Protocol-blind orchestration for registered vaults.
//!
//! [`Host`] owns registry semantics and the lifecycle of each vault entry. It
//! deliberately delegates vault and store effects through [`EntryOps`]: the
//! host decides *when* attach, reconciliation and detach happen, while the
//! filesystem and store crates remain the only owners of those effects.

mod lifecycle;
mod production;
mod registry;

pub use lifecycle::{
    Demand, DemandLease, EntryOps, Healing, Host, HostError, JobFailure, LifecyclePolicy,
    ProgressReporter, ReconcileWork,
};
pub use production::{
    MAX_CHANGESET_SIZE, ProductionEntryOps, ProductionPolicy, ProductionPolicyError,
};
pub use registry::{AliasConflict, ServingRegistry};

// `norn-embed` is a declared architecture edge that no module consumes yet.
// The witness keeps the dependency allowlist and manifest in agreement.
#[allow(unused_imports)]
use norn_embed as _;
