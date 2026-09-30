//! The plan vocabulary: what every write compiles to.
//!
//! **Every write is a plan.** A write verb is a plan of one operation, and
//! `apply` sends a plan a caller holds; both are planned and applied by the
//! one planner and the one applier, which live in the host. What is spelled
//! here is the data they exchange with a caller and nothing they do.
//!
//! **A caller holds one of two documents.** It authors operations, each a kind
//! and its fields, or it holds the resolved plan a preview answered with: the
//! operations again, one transition per file they touch — the state the file
//! must hold before the write and the state it holds after, each absent or a
//! content hash — the conditions the planning read, and the identity of the
//! vault's root. Re-sending a resolved plan finishes whatever a crash or an
//! I/O failure interrupted, because a file already at its after-state has
//! landed.
//!
//! **What a plan names, it names by grammar.** A content hash names its
//! algorithm and refuses any other; a root identity is an opaque string only
//! its constructor builds; an operation identifier is a string that names
//! something. A plan is read the way the host will act on it, so each of them
//! refuses at the read what it would refuse at construction.
//!
//! **A plan refuses what it does not know.** Every plan type refuses an
//! unknown key, where an answer drops one; the crate's conventions give the
//! reason.

pub(crate) mod document;
pub(crate) mod forecast;
pub(crate) mod hash;
pub(crate) mod operation;
pub(crate) mod outcome;
pub(crate) mod root;
pub(crate) mod value;
pub(crate) mod write_target;
