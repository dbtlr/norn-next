//! The store's black-box contract suite.
//!
//! Every case here reaches the store through its public API and never opens a
//! connection, which is the same rule every other crate in the workspace is held
//! to. Where a case has to arrange state the store would never write — a
//! desynchronized full-text index, a dropped index, a value outside a closed
//! vocabulary, a database that reports itself busy — it does so through
//! `norn_store::induced_failure`, which is the store's own fenced seam for
//! exactly that and the only way to reach the rungs and refusals from outside.
//!
//! The suites are modules of one binary rather than one binary each, so the
//! shared fixtures in [`common`] are used in full and nothing needs a
//! `dead_code` allow to hide what no case reaches.

mod common;

mod counters;
mod facts;
mod increments;
mod lifecycle;
mod pillars;
