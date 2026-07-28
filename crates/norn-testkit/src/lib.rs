//! Assertion helpers and harness scaffolding for the workspace's test
//! suites. Never ships.
//!
//! Helpers live here once; the suites that use them live with the subjects
//! they exercise. The corpus is the first of those: [`corpus`] holds the
//! activation gating, and the suite it gates is an integration test in the
//! `norn` bin package.

pub mod base64;
pub mod corpus;
