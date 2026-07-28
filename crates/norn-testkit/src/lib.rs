//! Assertion helpers and harness scaffolding for the workspace's test
//! suites. Never ships.
//!
//! Helpers live here once; the suites that use them live with the subjects
//! they exercise. The corpus is the first of those: [`corpus`] holds the
//! activation gating, and the suite it gates is an integration test in the
//! `norn` bin package.
//!
//! The measurement machinery is the same shape: [`counters`] compares counter
//! readings, [`explain`] states plan assertions over emitted SQL, [`scale`]
//! expresses the size-independence pair, and [`process`] spawns a child under
//! isolation and measures what it cost. Each is helpers only — the bars that
//! use them land with the subjects they measure.

pub mod base64;
pub mod corpus;
pub mod counters;
pub mod explain;
#[cfg(unix)]
pub mod process;
pub mod scale;
