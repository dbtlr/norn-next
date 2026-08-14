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
//! [`lanes`] is the third: it walks a crate's `tests/` directory and holds
//! every `#[ignore]` in it to the lane that adopts it. The walk lives here and
//! the tables live with the crates they describe, so a crate joining the lanes
//! gains the guard rather than a copy of it.
//!
//! [`generated`] is assistance rather than enforcement: it hands a suite the
//! documents a fixture profile generates, without the suite holding a
//! temporary tree to get them.
//!
//! [`equivalence`] is the judgment two derived stores are held to: it projects
//! every derived fact a store holds, compares two projections and names the
//! first field they disagree about, and states what each store answers for on
//! its own. It lives here because two crates ask that question of stores they
//! built by different routes, and one comparator is what keeps them from
//! drifting into two meanings of "the same derived state".
//!
//! [`fidelity`] is the seam that judgment is recorded through. A comparison
//! that only passes or fails answers one case and leaves no history, so every
//! comparison a suite makes can be written out as one record — the populations
//! it stood on and the first field it named. Nothing reads those records yet;
//! the seam exists so the drift scans and the post-lockdown comparison attach to
//! one vocabulary rather than inventing one apiece.
//!
//! [`churn`] is the other half of that judgment's subject. The comparator says
//! whether two derived stores agree; the churn driver is what puts one of the
//! two trees through the editing a host has to converge over — scripted, seeded
//! workloads a suite applies to a live tree, each step saying what it does so a
//! failing case prints its script. It manipulates trees and describes what it
//! left behind, and knows nothing of hosts or stores: the suite that attaches,
//! settles and judges lives with the host it exercises.
//!
//! [`attestation`] is what an induced-failure bar reads to know its arm fired.
//! A case about a process that dies is judged by a process that did not meet
//! the condition, and state at rest cannot tell a hook that fired from a hook
//! that was deleted — so each arm records itself where it fires and the parent
//! asserts on the record beside the outcome. The vocabulary and the reading
//! live here because two crates' seams write records a third crate's suites
//! read.
//!
//! [`invalidation`] is assistance of the same kind, for a judgment two suites
//! in different crates make about the same reports: whether a watcher's
//! invalidation root reaches a path. Answering it here is what keeps the two
//! from drifting into two meanings of one name.
//!
//! The measurement machinery is the same shape: [`counters`] compares counter
//! readings, [`explain`] states plan assertions over emitted SQL, [`work`]
//! holds a drain's engine-step count under a line in the rows it drained,
//! [`scale`]
//! expresses the size-independence pair, [`process`] spawns a child under
//! isolation and measures what it cost, and [`readings`] renders what a
//! measurement found and records it under the run. Each is helpers only — the
//! bars that use them land with the subjects they measure, because a bar is
//! authored against one subject and moves only by a reviewed edit.
//!
//! [`isolation`] holds the suites apart where the machine has one of
//! something. Its leases are cross-process, because the runner's own
//! parallelism spans processes: a case holding a real platform watcher
//! contends with every other binary the runner started, not only with its own
//! threads.
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
pub mod attestation;
pub mod base64;
pub mod churn;
pub mod corpus;
pub mod counters;
pub mod equivalence;
pub mod explain;
pub mod fidelity;
pub mod generated;
pub mod invalidation;
pub mod invariants;
pub mod isolation;
mod json;
pub mod lanes;
mod poll;
#[cfg(unix)]
pub mod process;
pub mod readings;
pub mod regression;
pub mod scale;
pub mod wait;
pub mod work;
