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
    Demand, DemandLease, EntryOps, Host, HostError, JobFailure, LifecyclePolicy, ProgressReporter,
    ReconcileWork,
};
pub use production::{ProductionEntryOps, ProductionPolicy, ProductionPolicyError};
pub use registry::{AliasConflict, ServingRegistry};

// These imports are boundary witnesses. The concrete substrate adapter lands
// beside the lifecycle kernel; declaring all six edges now is required by the
// architecture allowlist when this crate becomes present.
#[allow(unused_imports)]
use norn_embed as _;
#[allow(unused_imports)]
use norn_store as _;
#[allow(unused_imports)]
use norn_text as _;
