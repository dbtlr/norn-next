//! The one planner: a plan's operations in, a resolved plan and its forecast
//! out, or the fault in the plan's own shape.
//!
//! **What the planner owns.** Invariant 4 names one plan vocabulary and one
//! applier; this is the one path from operations to the transitions that
//! applier executes. A write verb's single operation, a caller's authored
//! plan, and the operations a refused apply re-resolves for its fresh plan all
//! plan here, through [`resolve::resolve`], so an operation means the same
//! thing wherever it arrives from. Planning is four steps, one module each:
//!
//! - [`order`] — the plan's shape: the order its operations compose in, and
//!   the faults `request/plan-invalid` answers — an identifier carried twice,
//!   a requirement nothing carries, a cycle of requirements, and a cycle of
//!   content, such as two documents exchanging places.
//! - [`compose`] — each file's after-bytes from its before-bytes and the
//!   operations touching it, in that order. An operation that cannot act on
//!   what it meets — an edit whose text does not occur exactly once, a create
//!   or a move onto a name something stands at — does not resolve; it is not
//!   a fault in the plan.
//! - [`resolve`] — the resolved plan: an operation that does not resolve is
//!   left out, with every operation that touches one of its files or requires
//!   it, and what remains is one transition per file, the author conditions on
//!   files the plan does not write, and the root's identity.
//! - [`forecast`](mod@forecast) — the folders the plan makes and removes.
//!
//! **Composition is shared with the applier.** A target's after-bytes are a
//! pure function of the before-bytes of the files the plan touches and the
//! operations, taken in the one order [`order::dependencies`] settles. The
//! planner hashes that result into each transition; the applier recomposes
//! with [`compose::compose`] and refuses unless the result hashes to the
//! after-state the plan carries.
//!
//! **Before-states are read from the files, not the store.** A before-state is
//! the hash of the bytes a target is composed from, so both sides read them
//! through a [`view::VaultView`]: the store holds no document's exact bytes,
//! and a hash read from it could name bytes nobody composed against. The
//! request's one snapshot is what an operation that reads derived facts plans
//! against; none of today's four kinds reads one.
//!
//! **What the planner leaves to the applier.** Whether a composed result
//! passes the vault schema is checked while every target is staged, as the
//! architecture's apply seam places it, so a preview answers without it.
//! A case-only rename on a root that folds case is not planned as one here:
//! the two spellings key two files, so the destination reads as taken and the
//! move does not resolve.
//!
//! **Memory.** A resolved plan carries its operations and one fixed-size
//! transition per target, never the bytes of a file it did not author.
//! Planning itself holds the bytes of every file the plan touches until it
//! answers.
//!
//! **A dormant carrier.** Its consumers are NORN-295's own later layers: the
//! applier, which recomposes every target and re-resolves operations through
//! this planner for refuse-and-refresh, and `Host::apply`, which plans an
//! authored plan inside the entry's claim and plans a preview on one
//! snapshot. Neither has landed, so nothing outside this module's own tests
//! reaches it yet, and its contract is held by those tests alone.

pub(crate) mod compose;
pub(crate) mod forecast;
pub(crate) mod order;
pub(crate) mod resolve;
pub(crate) mod view;
