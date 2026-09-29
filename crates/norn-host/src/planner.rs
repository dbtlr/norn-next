//! The one planner: a plan's operations in, a resolved plan and its forecast
//! out, or the fault in the plan's own shape.
//!
//! **What the planner owns.** Invariant 4 names one plan vocabulary and one
//! applier; this is the one path from operations to the transitions that
//! applier executes. A write verb's single operation, a caller's authored
//! plan, and the operations a refused apply re-resolves for its fresh plan all
//! plan here, through [`resolve::resolve`], so an operation means the same
//! thing wherever it arrives from. A refresh names the operations an earlier
//! apply already landed, which it drops, so a remaining operation's
//! requirement on one of them is met rather than a fault. Planning is four
//! steps, one module each:
//!
//! - [`order`] — the plan's shape: the order its operations compose in, and
//!   the faults `request/plan-invalid` answers — an identifier carried twice,
//!   a requirement nothing carries, a cycle of requirements, and a cycle of
//!   content, such as two documents exchanging places. A move or a create
//!   waits for what vacates its name only where a document other than its
//!   own source stands there at planning, so the vault decides whether a
//!   chain of moves is a cycle.
//! - [`compose`] — each file's after-bytes from its before-bytes and the
//!   operations touching it, in that order. An operation that cannot act on
//!   what it meets — an edit whose text does not occur exactly once, a create
//!   or a move onto a document, onto a folder or beneath a document — does
//!   not resolve; it is not a fault in the plan.
//! - [`resolve`] — the resolved plan: an operation that does not resolve, or
//!   whose author's condition the vault no longer meets, is left out, with
//!   every operation that touches one of its files or requires it, and what
//!   remains is one transition per file, the author conditions on files the
//!   plan does not write, and the root's identity.
//! - [`forecast`](mod@forecast) — the folders the plan makes and removes.
//!
//! **Composition is shared with the applier.** A target's after-bytes are a
//! pure function of the before-bytes of the files the plan touches and the
//! operations, taken in the order a resolved plan carries them, which is the
//! one [`order::dependencies`] settled. The planner hashes that result into
//! each transition; the applier recomposes with [`compose::compose`] and
//! refuses unless the result hashes to the after-state the plan carries.
//!
//! **A file is its identity.** Every name an operation carries is read
//! through `norn-fs`'s one path-spelling normalization point, which the
//! [`view::VaultView`] exposes under the case behavior the root proved: two
//! spellings of one file compose as one target, held at the spelling the
//! tree lists. On a root that folds case a destination differing from its
//! source only in case names the source itself (ADR 0031), so a case-only
//! rename plans as a move — the old spelling from present to absent and the
//! new one from absent to present — which the applier publishes as
//! `norn-fs`'s respell. The name it arrives at is the one it vacates, so it
//! waits for no other operation vacating that name, and an operation after it
//! in the plan — a removal, a move on, a rename back — acts on the document at
//! its new spelling. A folder's change of case is not planned: an operation
//! naming a folder in a case the tree does not list, or in a case other than
//! the one an earlier operation of the plan made it in, is left unresolved.
//!
//! **Before-states are read from the files, not the store.** A before-state is
//! the hash of the bytes a target is composed from, so both sides read them
//! through a [`view::VaultView`]: the store holds no document's exact bytes,
//! and a hash read from it could name bytes nobody composed against. The
//! request's one snapshot is what an operation that reads derived facts plans
//! against; none of today's four kinds reads one.
//!
//! **A create publishes before any removal.** ADR 0031 publishes creates
//! first and removals last, so a name a removal of this plan vacates still
//! stands when a create publishes: a create beneath a document the plan
//! removes, or at a folder the plan empties, does not resolve, while a move
//! or create onto a document another operation moves away or removes runs
//! after it and resolves.
//!
//! **What the planner leaves to the applier.** Whether a composed result
//! passes the vault schema is checked while every target is staged, as the
//! architecture's apply seam places it, so a preview answers without it.
//! Publishing a case-only rename's two transitions as one respell is the
//! applier's, as is checking the plan's conditions again, landed or not.
//!
//! **Memory.** A resolved plan carries its operations and one fixed-size
//! transition per target, never the bytes of a file it did not author.
//! Planning itself holds the bytes of every file the plan touches until it
//! answers.
//!
//! **A dormant carrier.** Its consumers are NORN-295's own later layers: the
//! applier, which recomposes every target and re-resolves operations through
//! this planner for refuse-and-refresh, and `Host::apply`, which plans an
//! authored plan inside the entry's claim over a [`view::TreeView`] and plans
//! a preview on one snapshot. Neither has landed, so nothing outside this
//! module's own tests reaches it yet, and its contract is held by those tests
//! alone, one of them over a tree on disk.

pub(crate) mod compose;
pub(crate) mod forecast;
pub(crate) mod order;
pub(crate) mod resolve;
pub(crate) mod view;
