//! The vault filesystem seam's contract suite.
//!
//! Every case reaches the crate through its public API, over a temporary vault
//! and a temporary shadow home. Nothing here reads an environment variable and
//! nothing here writes outside the tree it made.
//!
//! The suites are modules of one binary rather than one binary each, so the
//! scaffolding in [`common`] is used in full and nothing needs a `dead_code`
//! allow to hide what no case reaches. The maintainer lock has a binary of its
//! own — `tests/maintainer.rs` — because its cases spawn processes, and a case
//! that spawns nothing should not wait on one that does.
//!
//! Two claims are checked from inside the crate rather than here, and both for
//! the same reason: they need a stage of the protocol to fail, or a foreign
//! writer to land in a window one call wide. Those are `src/write.rs`'s own
//! unit tests, which reach the injection seams this file cannot.

mod common;

mod adjacent;
mod preconditions;
mod publication;
mod shadows;
