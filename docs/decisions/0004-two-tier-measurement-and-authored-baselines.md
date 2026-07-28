# 0004 — measurement divides by lane, and a trend's whole memory is a checked-in baseline

Both CI lanes existed as structure before either measured anything. This
decision records what the division means now that both carry real work:
which lane a given assertion belongs in and why, what a trend is made of
when there is no trend store, and why the first memory bar's subject is a
fixture generator rather than the thing the invariant is ultimately about.

The invariant spine already states the rule — counters and structure gate
per pull request; clocks and trends live in the soak lane and never fail
one. What follows is that rule made operational, plus the two mechanisms it
needed before it could be practised.

## The contract

**Peak memory is a count, so its bar gates per pull request.** A number of
bytes compared against a fixed number of bytes is the same kind of
assertion as a counter reading: it does not move because a runner was
busy. The per-PR memory job spawns the generator as a child, reads the peak
resident set the kernel accounted to it, and holds it to a ceiling at the
~2k-document profile. Nothing in that lane reads a clock. The
`timeout-minutes` on those jobs are not an exception — a timeout is a
runaway guard, set loose enough that a job approaching it is broken rather
than slow.

**A wall-clock bar in the soak lane is a sanity ceiling, not a gate on
speed.** A hosted runner's throughput varies by more than any regression
worth catching, so a bar tight enough to catch one would fail for reasons
that are not about this repository. The soak ceiling is set where a run past
it is broken — roughly fifty times the local reading — and **the value the
lane produces is the recorded number, not the pass**. That is the whole
reason clocks are legal here and nowhere else: this lane reports, and the
per-PR lane judges.

**The lanes divide by kind, not by stopwatch.** The ≥5k-document profile is
the soak lane's work whatever it costs to run, which is
[ADR 0002](0002-fixture-determinism-and-calibration.md)'s split applied
rather than re-decided. The consequence is paid rather than dodged: the
2k-versus-5k memory pair bills under ten seconds and would sit comfortably
in the per-PR lane, and it is a soak case anyway. Splitting on lane
discipline keeps the rule from drifting every time a machine gets faster,
and it is what the spine means by putting the flat-slope requirement in the
scheduled lane.

**A trend's memory is a checked-in baseline, and nothing else.** No
artifact history is fetched, no external store is consulted, and no run
compares itself to what a previous run happened to record. Three grounds:
a lane that reads its own history ratifies whatever it did last, so a slow
drift becomes the new normal without anybody deciding it; a baseline in the
tree is a diff a reviewer reads; and artifact retention is a hosting policy
that expires, which is a poor place to keep a contract. This is
ADR 0002's authored-envelope stance carried from tree shape to runtime
cost, and it is the same discipline in both places: **a number moves when
somebody changes it in a diff, with the grounds beside it.**

**Peak-memory baselines are ratchets; the wall-clock one is not.** Lowering
a memory ceiling needs no new argument. Raising one is a claim that the
subject now costs more, made in the open. The wall clock has no ratchet
because it has no trend worth defending against a runner's variance — its
recorded readings are the trend, and the ceiling only says the run
happened.

**Every soak reading is written where a person will find it.** Each
measurement appends its numbers to the run's job summary as well as to the
log. A green lane that leaves no number behind is a pass with nothing
behind it, and the lane exists for the numbers.

**A bar's subject is whatever exists; the slot names what will hold it.**
The generator carries the first memory bar because it is the only subject
this workspace has — a program whose cost can be measured against a tree it
made itself. `norn-store` binds at Layer 1 and its request paths become
what the job measures then; the ~5k scale and the memory trend stay in the
soak lane throughout. Where no subject exists at all, the job stays a
placeholder that says so and names the layer that fills it — the counter
and size-independence bars with `norn-store`, the `EXPLAIN` bars with the
Layer 3 read builders — because
[ADR 0003](0003-boundary-enforcement-harness.md)'s rule holds here too: the
machinery lands early, the bars land with their subjects.

**A measurement that measured nothing fails.** Each memory case requires
the generator's own report to carry the profile's document count before the
cost is read, and the two-scale pair fails when either side reported no
peak at all. A cheap reading from a run that did no work is the failure
mode a cost bar is most likely to have and least likely to notice.

**A soak step selects cases by kind, not by name.** A test-name filter that
stops matching runs zero tests and reports success, so each step asks a
suite for its ignored cases instead of naming them. The `#[ignore]` reason
is what says which lane a case belongs to, and it is the only place that
says it.

## Consequences

- Nothing measures a wall clock on a pull request, so a slow runner never
  fails one. The cost is that a genuine slowdown is visible only the next
  night, in a recorded number rather than a red check.
- A regression in peak memory fails the per-PR lane at the ~2k profile and
  the soak lane at both scales. The two-scale ratio is the sharper of the
  two instruments: a ceiling passes anything that fits under it, and a
  ratio fails the moment the scales stop moving together.
- The baselines file is a small, dull, frequently-read diff. That is the
  intended shape: recalibrating runtime cost is an act somebody performs,
  the way recalibrating tree shape already is.
- **Writing the first bar changed its subject.** The generator drew every
  clutter file a profile asked for before writing any of them, so its peak
  was the tree's total bytes — about 55 MiB at the ≥5k profile against
  about 5 MiB once each file is written as it is drawn. A bar authored
  against the earlier shape would have been a bar on a defect, and the
  flatness claim it was meant to carry would have been false: the ratio
  across the two scales was 5.7 and is now about 1.5.
- The soak lane's assertions are new, so the five consecutive green runs
  that lockdown counts start at the first run under them. An earlier green
  run of a lane that asserted nothing is not one of the five.
- The schedule is a thing to verify rather than assume: hosted schedules
  are disabled on repositories with no activity for sixty days, and a lane
  that silently stopped running looks exactly like a lane with nothing to
  report.
- A profile larger than ≥5k is a constant edit to the profile table when
  one is wanted. Nothing here is parameterised for a scale nobody has asked
  for.

## Evidence

- [`architecture.md`](../architecture.md) states the invariant spine's
  memory invariant and the lane rule this decision practises: per-PR CI
  asserts peak memory at a realistic ~2k profile; the scheduled lane
  carries the trend line and the flat-slope requirement. That document
  governs; this one records how the rule is met.
- [ADR 0002](0002-fixture-determinism-and-calibration.md) is where the
  authored-envelope stance and the by-kind lane split come from, and where
  the generator's determinism contract makes a repeated measurement a
  statement about the program rather than about the day.
- [ADR 0003](0003-boundary-enforcement-harness.md) is the process harness
  these bars are written with — the sandboxed spawn, the artifact copied
  before it is executed, and the peak resident set read from the kernel's
  accounting when the child is waited on rather than sampled — and the
  source of the rule that machinery lands before its bars.
- The baselines are set from readings of the pinned toolchain's unoptimized
  build on two platforms, both recorded beside the values: the ~2k profile
  peaks at about 3.0 MiB on linux and 4.3 MiB on macOS, the ≥5k profile at
  about 4.7 and 6.2, and the ≥5k generation bills about 3.2 seconds on
  either. The ceilings sit well above the higher reading of each pair,
  because a bar that flakes teaches people to rerun rather than to look.
- The zero-test hazard is not hypothetical: a mistyped name filter passed
  in under a second having run nothing, which is what moved the workflow
  steps off name filters.
