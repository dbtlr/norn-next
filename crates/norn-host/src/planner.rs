//! The one planner: a plan's operations in, a resolved plan and its forecast
//! out, or the fault in the plan's own shape.
//!
//! **What the planner owns.** Invariant 4 names one plan vocabulary and one
//! applier; this is the one path from operations to the transitions that
//! applier executes. A write verb's single operation, a caller's authored
//! plan, and the operations a refused apply re-resolves for its fresh plan all
//! plan here, so an operation means the same thing wherever it arrives from.
//!
//! **Composition is shared with the applier.** A target's after-bytes are a
//! pure function of the before-bytes of the files the plan touches and the
//! operations, taken in the one order the plan's requirements settle. The
//! planner hashes that result into each transition; the applier recomposes
//! with the same function and refuses unless the result hashes to the
//! after-state it carries.
//!
//! **A dormant carrier.** Its consumers are NORN-295's own later layers: the
//! applier, which recomposes every target and re-resolves operations through
//! this planner for refuse-and-refresh, and `Host::apply`, which plans an
//! authored plan inside the entry's claim and plans a preview on one
//! snapshot. Neither has landed, so nothing outside this module's own tests
//! reaches it yet, and its contract is held by those tests alone.

mod compose;
mod order;
mod view;
