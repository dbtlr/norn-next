//! Assertion helpers and harness scaffolding for the workspace's test
//! suites. Never ships.
//!
//! Helpers live here once; the suites that use them live with the subjects
//! they exercise. The corpus is the first of those: [`corpus`] holds the
//! activation gating, and the suite it gates is an integration test in the
//! `norn` bin package. [`regression`] is the second, and gates the same way:
//! it holds the registry of named defect classes and the dormancy rule that
//! decides which of them are carried by tests today.
//!
//! Two of these modules are enforcement rather than assistance.
//! [`architecture`] holds the dependency allowlist and the gate that compares
//! the workspace to it; [`invariants`] holds the mapping from each boundary
//! invariant to what carries it — an allowlist edge, a lint rule, or a
//! judgment made in review. That mapping is the authoritative one, and it is
//! code so that it is checked rather than read.
//!
//! The measurement machinery is the same shape: [`counters`] compares counter
//! readings, [`explain`] states plan assertions over emitted SQL, [`scale`]
//! expresses the size-independence pair, and [`process`] spawns a child under
//! isolation and measures what it cost. Each is helpers only — the bars that
//! use them land with the subjects they measure.

pub mod architecture;
pub mod base64;
pub mod corpus;
pub mod counters;
pub mod explain;
pub mod invariants;
mod json;
#[cfg(unix)]
pub mod process;
pub mod regression;
pub mod scale;
