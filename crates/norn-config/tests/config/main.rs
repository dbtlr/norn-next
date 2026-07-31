//! The machine-local state contract suite.
//!
//! Every case reaches the crate through its public API, over temporary
//! directories injected at construction. Nothing here reads or writes an
//! environment variable: that is one entry point, and it is exercised in
//! `tests/environment.rs`, which is a separate binary so that a process-wide
//! write cannot land beside a case that is not expecting one.
//!
//! The suites are modules of one binary rather than one binary each, so the
//! scaffolding in [`common`] is used in full and nothing needs a `dead_code`
//! allow to hide what no case reaches.

mod common;

mod concurrency;
mod layout;
mod names;
mod registry;
mod tolerance;
