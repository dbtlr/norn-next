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
//! [`generated`] is assistance rather than enforcement: it hands a suite the
//! documents a fixture profile generates, without the suite holding a
//! temporary tree to get them.
//!
//! The measurement machinery is the same shape: [`counters`] compares counter
//! readings, [`explain`] states plan assertions over emitted SQL, [`scale`]
//! expresses the size-independence pair, and [`process`] spawns a child under
//! isolation and measures what it cost. Each is helpers only — the bars that
//! use them land with the subjects they measure.
//!
//! Two modules bound how long a suite may wait, and they divide by subject:
//! [`process`] bounds a child process's run, and [`wait`] bounds an
//! in-process wait for state to converge. They agree on what a bound is worth
//! — every wait carries one, passing it reports what was seen rather than that
//! time ran out, and neither offers a wait without one — and they poll to one
//! cadence,
//! which is why that cadence lives in one private module rather than in each.
//!
//! They differ on where the number comes from, because what they bound is not
//! equally preemptible. A run's bound has a default:
//! [`process::DEFAULT_WAIT_DEADLINE`] is what a [`process::Run`] gets when its
//! call site says nothing, and the harness enforces it by killing the child it
//! spawned. A condition wait has no default and cannot have one:
//! [`wait::Budget`] has no `Default`, every call site declares its own, and the
//! bound observes rather than preempts, since the probe runs on this thread and
//! there is nothing to kill. A bound nobody wrote, over an evaluation nothing
//! can stop, is a wait no call site can tune.

pub mod architecture;
pub mod base64;
pub mod corpus;
pub mod counters;
pub mod explain;
pub mod generated;
pub mod invariants;
mod json;
mod poll;
#[cfg(unix)]
pub mod process;
pub mod regression;
pub mod scale;
pub mod wait;
