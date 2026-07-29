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
resident set the kernel accounted to it, and asserts two things: a ceiling
at the ~2k-document profile, and the ratio between the 300-document profile
and the ~2k one. Nothing in that lane reads a clock. The `timeout-minutes`
on those jobs are not an exception — a timeout is a runaway guard, set
loose enough that a job approaching it is broken rather than slow.

**The per-PR lane carries a flatness pair of its own, because a ceiling is
the blunter instrument.** A ceiling passes anything that fits under it, so
it can only be set where it forbids the shape it was authored against — and
a shape that costs the tree's total bytes is only expensive at a large
tree. A ratio between two scales fails the moment the scales stop moving
together, and needs neither of them to be large. The pair the per-PR lane
runs is `ambiguous` against `realistic`: 300 documents against 2000, with
35x of clutter between them, and both under 5k so both are this lane's work
by kind. The ratio is a count like the ceiling is — a number of bytes over
a number of bytes — so nothing about putting it here reads a clock.

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
scheduled lane. The per-PR lane gets the sharp instrument by using scales
it already owns rather than by borrowing a soak profile.

**A lane runs a whole test target, so lane membership is a target
boundary.** A step asks a suite for its ignored cases wholesale and cannot
ask for some of them, so two lanes' cases in one binary means each lane
runs both. The memory cases are therefore two targets — one per lane —
rather than two `#[ignore]` reasons in one, which is what keeps the soak
wall-clock assertion out of the per-PR lane.

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

**What binds is the comparison; the direction a baseline travels is
review-held.** A reading past a baseline fails the run — that much is
mechanical. Nothing forbids raising the baseline afterwards. Lowering one
needs no new argument; raising one is a claim that the subject now costs
more, and what asks for that claim is a reviewer reading a small, dull,
frequently-read diff. This is named as review-held rather than left implied,
because a review-held invariant rots quietly. **The mechanized version
arrives with the Layer 2 lockdown work**, when this lane carries a host
workload and a memory slope over a nightly mixed load becomes a thing a run
can measure rather than a thing a diff asserts.

**Every reading is written where a person will find it.** Each measurement,
in either lane, appends its numbers to the run's job summary as well as to
the log. A green lane that leaves no number behind is a pass with nothing
behind it, and the readings are what the downward discipline is exercised
against.

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
the generator's own report to carry the profile's document count — parsed
as a number, so `6000 documents` is not satisfied by `16000 documents` —
before the cost is read, and a flatness pair fails when either side
reported no peak at all. A cheap reading from a run that did no work is the
failure mode a cost bar is most likely to have and least likely to notice.

**A lane step asserts that cases ran, and selects them by kind rather than
by name.** These are two mechanisms against one hazard, and only the first
closes it. Selecting by kind — asking a suite for its ignored cases instead
of naming them — removes the spelling where a mistyped name filter matches
nothing; it does not help at all when the cases themselves are gone,
because `--ignored` with nothing to match still exits 0. So every lane step
goes through one script that captures the run's output, gates on its exit
status, and then requires a `test result: ok` line reporting at least one
pass. **The pass-count assertion is what closes the hazard**, and it is one
script rather than a block copied into each step so that it cannot close it
in three places and not the fourth.

**A lane's `#[ignore]` reason is checked, not merely conventional.** The
reason is what says which lane a case belongs to, so an `#[ignore =
"flaky"]` added for a local reason would be silently adopted by whichever
lane runs its suite and would face a bar nobody meant it to face. A test in
the fixtures suite reads the crate's own test sources and requires every
ignore reason to open with a sanctioned lane prefix.

## Consequences

- Nothing measures a wall clock on a pull request, so a slow runner never
  fails one. The cost is that a genuine slowdown is visible only the next
  night, in a recorded number rather than a red check.
- A generation whose cost is the tree's total bytes fails the per-PR lane
  twice over: past the ~2k ceiling, and past the 300-versus-2k ratio bar.
  Both were checked against that shape rather than reasoned about — the
  ceiling holds it out by about 5% at its narrowest reading and the ratio by
  better than 1.5x, which is why the pair is the instrument the claim rests
  on and the ceiling is the coarse guard beside it.
- The baselines file is a small, dull, frequently-read diff. That is the
  intended shape: recalibrating runtime cost is an act somebody performs,
  the way recalibrating tree shape already is. It is also the whole of the
  downward discipline, which is why that discipline is named review-held
  above rather than described as a mechanism.
- **Writing the first bar changed its subject.** The generator drew every
  clutter file a profile asked for before writing any of them, so its peak
  was the tree's total bytes — about 52–55 MiB at the ≥5k profile against
  about 5 MiB once each file is written as it is drawn. A bar authored
  against the earlier shape would have been a bar on a defect, and the
  flatness claim it was meant to carry would have been false: the ≥5k-to-2k
  ratio was 5.6–6.2 and is now 1.32–1.71.
- Lockdown counts five consecutive green runs of the soak lane over a
  nightly ~1h mixed load, showing zero counter violations, a flat memory
  slope and no file-descriptor growth. Three of those terms have no subject
  in this workspace yet, so the count does not start at the first green run
  of the assertions this decision adds: it starts when the lane asserts what
  lockdown counts.
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
  asserts peak memory at a realistic ~2k profile; the scheduled lane bars
  each run's reading against the authored baseline, with the baseline's
  downward movement review-held until the Layer 2 flat-slope requirement
  mechanizes it. That document governs; this one records how the rule is met.
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
  build on two architectures, both named beside the values: **linux-arm64**
  under docker and **macos-arm64** natively, 6 to 8 runs per profile per
  architecture. The bands are 2.79–3.11 and 3.80–4.16 MiB at the ~2k
  profile, 4.55–4.76 and 5.48–5.89 MiB at the ≥5k profile, and 3.19–3.54 and
  3.58–3.61 seconds for the ≥5k generation's wall clock. The ceilings sit
  well above the higher band, because a bar that flakes teaches people to
  rerun rather than to look. **The CI runner is x86_64-glibc, which is
  unmeasured** until the first `ubuntu-latest` run; the headroom is what
  covers an allocator these readings do not describe, and the first run
  there is what turns two readings into three.
- The bars were checked against the shape they forbid rather than against
  argument alone. A build holding its clutter set peaks at 8.40–8.67 MiB on
  linux-arm64 and 9.50–9.73 MiB on macos-arm64 at the ~2k profile, against
  an 8 MiB ceiling; its 300-versus-2k ratio is 3.12–3.70 against a 2.0 bar,
  and its ≥5k-to-2k ratio 5.60–6.17 against a 2.2 bar.
- The zero-test hazard is not hypothetical: a mistyped name filter passed
  in under a second having run nothing. That moved the workflow steps off
  name filters, which was necessary and not sufficient — `--ignored`
  matching nothing exits 0 just as quietly, and the pass-count assertion is
  what makes the step fail.
