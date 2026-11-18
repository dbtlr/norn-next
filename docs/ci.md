# CI coverage and regression evidence

Ordinary functional tests remain required on pull requests, including doctests and
feature-enabled host cases. Stable counts and structure are correctness evidence.
Clocks and trends belong to the dormant soak tier under
[ADR 0004](decisions/0004-two-tier-measurement-and-authored-baselines.md).

## PR and full-regression coverage

`ci.yml` runs six jobs on PRs, main pushes and manual runs. Ordinary default
workspace tests and doctests split by package across three of them. `build and test`
runs everything except `norn-host`, `norn-testkit` and `norn-serve`.
`build and test (norn-host and norn-testkit)`, whose required name predates the
current package list, runs `-p norn-testkit -p norn-serve`. `host functional
(induced-failure)` runs `-p norn-host --features induced-failure`. The catch-all uses
`--workspace --exclude norn-host --exclude norn-testkit --exclude norn-serve`, so new
crates join it. The package lists must match exactly, and every package's ordinary tests
and doctests run on PRs in exactly one of the three jobs.
`norn-serve` sits in the named shard because its tokio and hyper switch features on in
`mio` and `smallvec`, which `norn-host`'s graph links.

`norn-host` runs once on PRs, with `induced-failure` on. Every case its
default-feature run executes also runs with the feature (2,197 cases in both runs,
none only in the default run), and its `compile_fail` doctests do not depend on the
feature. The feature-off build stays proven on every PR by compilation: the build job
builds the shipped graph, and the workspace clippy compiles `norn-host`'s targets with
the feature off. The nightly `regression.yml` still runs the default-feature suite for
the whole workspace, `norn-host` included, so a break only the feature-off run shows is
caught within a day.

The two package shards, `build` and `build-host`, each build tests with the
whole-workspace feature set. `norn-host`'s development dependencies switch on the fault
features of `norn-fs`, `norn-store` and, through the store, `norn-db`, and the named shard
no longer selects `norn-host`. `norn-serve` therefore names `norn-fs` (`test-support`,
`induced-failure`) and `norn-store` (`induced-failure`) as development dependencies, so
the shard builds those packages with the set the whole-workspace run gives them. The architecture job enforces both claims with
`cargo test -p norn-testkit --test shards`: it reads each job's selection from `ci.yml`,
resolves features with `cargo tree` (normal, development and build edges, host target)
for the whole workspace and for each job, and fails naming the job and package when a
package is selected in no PR job or in two, and naming the shard, package and feature
when a package shard builds any package in its graph, selected root or dependency, with
a different feature set. The host functional job joins the first check only: it
sets `induced-failure` on purpose, and its command may carry only `--features` and the
one SQLite `--config` value below. A step run through `test-suite.sh` is a focused
step, not its job's package step, and the guard leaves it out of the partition: the
host functional job's second step runs the `norn` drain cases that hold a real host's
apply through `norn-fs`'s write seam, which only `norn`'s own `induced-failure` feature
compiles into the binary.

The host functional job builds the bundled SQLite (`libsqlite3-sys`) at
opt-level 3 by passing `--config profile.dev.package.libsqlite3-sys.opt-level=3`
to its `cargo test` command. Cargo does not read a per-package profile override
from the environment, so the flag is how a single run sets it. The job's
`rust-cache` step has the extra key `sqlite-opt3`, which separates its cache from
the ones saved before the flag, so the optimized SQLite object is saved.

The other PR jobs do not pass the flag. Measured on hosted runners over three
warm runs each, it shortened the host functional job by about a quarter and left
the two package shards within 3% of `main`. The macOS job, which would also have
to build the crates above SQLite a second time for the folding-root spelling
bar, was 7% slower.

Every nightly correctness lane runs without the flag, so its measurements stay at
opt-level 0. The nightly performance script instead builds release subjects and
verifies their optimized profiles in its build receipts. Lane steps cannot carry it, because `lane-suite.sh` builds its own
`cargo test` command. The workflow reader accepts this one exact flag and value
and refuses every other `--config`.

| PR job | Evidence |
|---|---|
| Build and test | Workspace build, default tests and doctests except norn-host, norn-testkit and norn-serve, cheap registry structure and dormancy checks |
| Build and test (norn-host and norn-testkit) | Default norn-testkit and norn-serve tests and doctests; cargo test builds its selected targets |
| Lint and document | Format, all six clippy contexts, performance-harness unit tests, the token counter bar and the counters route's unit test under `counters`, denied-warning rustdoc |
| Host functional (induced-failure) | The only PR run of norn-host's ordinary tests and doctests, feature-enabled, including churn and rolling-heal correctness and work counters |
| macOS platform | Embedding determinism, filesystem contracts, Obsidian parity, exact case-rename and unreadable-document/subtree cases, folding-root spelling bar |
| Architecture | Actionlint, the architecture gate and the shard certification |

`regression.yml` runs the complete nine-job matrix at 02:17 UTC daily and by
manual dispatch. Its `full ` job names retain the build, induced-failure,
platform, folding-counter, architecture, Linux counter, Linux memory and the two
nightly performance jobs, current and serve.
Compiled registry-carrier and inventory audits run only in its Linux build job.
The complete lockdown and process-death recovery targets run in its Linux
induced-failure and macOS platform jobs. Counter and memory lanes run in full
regression; the focused PR macOS spelling bar also runs `folding_cost`.

Fault and compiled-audit cases use `fault-lane case:` and `audit-lane case:`
ignore reasons. Lane tables and the workflow reader require wholesale adoption
through `lane-suite.sh`, with the features each target needs. The cheap registry
check still validates structural and mandatory sets and stale dormancy without
building a compiled index. The dormant certification runner includes ignored fault
carriers; reconciliation still rejects other ignored certification cases. Exact macOS native cases use `test-suite.sh` with
`--exact --include-ignored`, so moving a fault case cannot silently skip it.

Full jobs keep their profiles, features, authored assertions and runaway guards.
macOS uses `macos-15` and asserts ARM64. Host fault and counter lane steps name
`LANE_FEATURES: induced-failure`. The full Linux memory job keeps its 20-minute
guard and existing feature selections.

Two independent jobs measure performance, `full nightly performance (current)`
and `full nightly performance (serve)`. Each records one engine with
`nightly.py --engines <engine>` and uploads `nightly-performance-<engine>-<attempt>`.
The script builds independently receipted release subjects outside the
workspace target, so neither job needs build-job artifacts or waits on another
job. Each has its own checkout, Rust cache and `regression-<job>-<attempt>`
identity receipt. Each job's summary sets the stored legacy record beside its
engine or says why it cannot, as `docs/performance.md` describes. Complete
release evidence requires both jobs and both performance artifacts as well as
the other seven jobs. An artifact can contain partial diagnostics after an
engine fails; its presence alone does not qualify a run. A complete current
artifact contains `current/record.json` and `summary.md`, and a complete serve
artifact contains `serve/record.json` and `summary.md`.

Legacy Norn 0.48.3 is not recorded nightly. The dispatch-only `legacy-record.yml`
records it in six parallel `part` jobs, one process of one size each, and an
`assemble` job that uploads the record to commit as
`scripts/performance/legacy-record.json`. It is no part of release evidence.

Each performance guard is a runaway bound over completed hosted runs, and
`nightly.py` gives each engine's recorder a deadline inside it, so a stuck
engine still packages its evidence before the job is cancelled:

| Job | Guard | Completed runs | Recorder deadline |
|---|---:|---|---|
| `full nightly performance (current)` | 30 min | 6m47s and 4m43s; each process 25 to 41 s | 60 s + 10 s per 1,000 documents per process: 12 min |
| `full nightly performance (serve)` | 45 min | 14m08s and 12m02s; each process 86 to 103 s, 60 s of it the idle wait | 180 s + 10 s per 1,000 documents per process: 24 min |
| `legacy-record.yml` part, 2,000 documents | 90 min | 16m57s to 18m07s; the process 14m49s to 15m34s | 600 s + 1,600 s per 1,000 documents: 63 min |
| `legacy-record.yml` part, 10,000 documents | 300 min | 1h36m12s to 1h57m08s; the process 1h33m42s to 1h54m04s | 600 s + 1,600 s per 1,000 documents: 4 h 37 min |

Each job also builds its release subjects first, about 3 minutes for the
current worker and 1.5 more for the serving binary. Serve's 60 s idle wait is
part of what it measures, the first Get after an observed idle detach. It is
not shortened: the idle clock starts while a from-zero build can still be
running, so a shorter wait could detach the vault before the warm calls.

Every suite runs through the flake tripwire, which records ledger matches without
retrying or changing verdicts. Ignored targets use `lane-suite.sh`, whose shared
`test-suite.sh` rejects an empty successful run and preserves command failure.
Whole-workspace invocations may legitimately contain empty binaries and do not use
that single-target guard.

Full regression uses Cargo's `--no-fail-fast`. After identity succeeds, widening
`!cancelled()` conditions let later independent suites collect evidence while an
earlier failure stays red. An identity failure skips the build and test steps;
its failed receipt is still uploaded. Failed compilation or missing prerequisites can leave obligations
unexecuted; they supply no passing evidence.

## Candidate identity and manual runs

Each full job checks the actual Git `HEAD` against the captured event SHA, workflow
SHA and expected candidate. All four must be the same lowercase 40-character SHA.
The optional `candidate` input asserts identity; it never selects another checkout.
There is no checkout `ref` override. The workflow definition and tested tree come
from the same captured commit.

After this workflow exists on the default branch, dispatch a branch or tag that
holds the intended candidate:

```sh
gh workflow run regression.yml --ref <branch-or-tag> -f candidate=<full-candidate-sha>
```

An advancing branch or incorrect selector fails identity rather than qualifying a
different candidate. For a scheduled run, the expected candidate is the captured
main commit. Each job uploads `regression-<job>-<attempt>` containing an identity
receipt, including the actual SHA, workflow SHA, job, run ID, attempt and platform.
An identity receipt alone does not establish that tests passed.

One full run executes at a time. Running work is retained; GitHub can replace a
pending request. A cancelled candidate supplies no evidence and needs a deliberate
new dispatch. Coverage does not promise testing every intermediate main commit.
Scheduled runs can be delayed or dropped, so a cron declaration is not proof of a
completed daily run. See [GitHub workflow events](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#schedule).

## Release verification and reuse

A release needs a successful complete `regression.yml` run for its exact candidate.
A compatible nightly result for the same SHA may supply it; otherwise dispatch the
candidate immediately. This workflow does not publish a release.

Inspect the recent runs and selected run:

```sh
gh run list --workflow regression.yml --limit 10 --json databaseId,headSha,createdAt,status,conclusion,url
gh run view <run-id> --json headSha,attempt,status,conclusion,jobs,url
gh run download <run-id> --pattern 'regression-*-<attempt>' --dir <evidence-directory>
```

Verify all of the following before accepting release evidence:

- The same run attempt completed successfully with all nine expected full jobs.
  Their names are the complete matrix names prefixed with `full `.
- Every required job and step executed its obligations successfully. Missing,
  skipped or cancelled required evidence does not qualify.
- All nine receipts identify their expected job and the same run ID and attempt.
  Each has `identity_matches: true` and candidate, event, workflow and checkout
  SHAs equal to the intended candidate.
- The suite contract is `active-regression-v1`; the candidate's checked-in commands,
  profiles, features and platforms satisfy the required verification contract.
  Retain the receipts and job results together. Missing or expired evidence needs
  a new qualifying run.
- Known release-blocking failures affecting the candidate have an explicit
  disposition. An unrelated older green commit does not clear them.

The full matrix includes all build, lint, documentation and architecture checks;
PR merge-ref results are not substituted for exact-candidate evidence. The dormant
pinned-candidate soak/certification campaign remains a separate obligation.

## Failure response and adoption

Ordinary full-regression failures block releases and require a tracked, owned fix.
Unrelated feature merges may continue after their own required checks pass.
Confirmed corruption, loss of authored bytes or violated write/refusal safety can
stop merges; triage records that classification and what clears it. Diagnose red
runs before rerunning. A runner failure or missed nightly supplies no positive
regression evidence. Repairs may dispatch a full run immediately.

The `main` ruleset requires these checks:

- `build and test`
- `build and test (norn-host and norn-testkit)`
- `lint and document`
- `host functional (induced-failure)`
- `determinism and filesystem (macOS ARM64)`
- `architecture gate`
- `CodeRabbit`

Full-run names stay distinct so a manual regression run cannot alias a
PR-required context.

## Review bots

`CodeRabbit` is a required context, but its review is best-effort. A completed
`CodeRabbit` check run that concludes success, neutral or skipped satisfies the
ruleset; a rate-limited review usually posts neutral. A rate-limited CodeRabbit can post no check run at all, and
the ruleset then refuses the merge. Before calling a PR ready, confirm that the
head's `statusCheckRollup` holds a `CodeRabbit` entry. If the entry is missing,
ask once with `@coderabbitai review`, or `@coderabbitai full review` when the
head was already reviewed, and do not hold a PR whose own checks and review
passed for a full review slot.

Kody is not a required context. It skips a PR whose base is not `main` or whose
check ran while the PR was a draft; `@kody start-review` asks again. Read and
answer every finding from both bots before merging, including CodeRabbit's
comments outside the diff.

## Local runs

`scripts/gate.sh` runs what a pull request's Linux CI runs. It executes each
`run:` step of every job in `.github/workflows/ci.yml` whose `runs-on` is
`ubuntu-*`, in file order, with the environment the workflow layers over each
step. It skips `uses:` steps (checkout, the cache, the actionlint container) and
every job on another runner, so the macOS job is not run. The commands come
from the workflow when the script runs, through the `norn-gate-steps` binary of
`norn-testkit`, which reads the workflow with a reader that shares the CI
guards' env layering. A step added to `ci.yml` therefore joins the gate with no
edit to the script. Each step runs under `bash -e`, as CI runs a step that
names no shell. What the script cannot honor as CI does stops it with an error:

- a step with an `if:`, `continue-on-error`, a `working-directory`, a `shell`,
  a `${{ }}` expression in its command or environment, or a `run` that is not
  a string;
- a step whose command names a runner file: `GITHUB_ENV`, `GITHUB_PATH`,
  `GITHUB_OUTPUT`, `GITHUB_STATE` or `GITHUB_STEP_SUMMARY`. The runner reads
  those files back for later steps and nothing does locally. A script the
  command calls is not read, so one that writes a runner file is not caught;
- a job with an `if:`, `continue-on-error`, `container`, `services`, `needs`,
  `strategy` or `defaults.run`, or a `runs-on` that is missing or is not a plain
  string;
- a workflow with `defaults.run`, and an `env` at any level that cannot be read
  whole.

A job's `timeout-minutes` is neither honored nor refused. In CI it is a runaway
guard that cancels a hung job; the script sets no deadline, so a step that
hangs locally hangs the gate until it is interrupted, and a job that would
outrun its guard in CI is not failed for it locally.

Each step prints its job and name, then PASS or FAIL with its exit status and
wall time. The gate keeps going after a failure, ends with a summary of every
step, and exits non-zero if any step failed. `--list` prints the steps and
their environment without running them, `--job <key>` runs one job, and
`--step <number|name>` runs one step (a name that stands in more than one job
needs `--job`), so an edit to the script or the workflow
can be checked without a full run.

The script holds no machine policy: it writes no absolute path and takes no
lock. The caller supplies `CARGO_TARGET_DIR`, `TMPDIR`, `CARGO_BUILD_JOBS`, the
toolchain on `PATH` and any lock that serializes heavy cargo runs, for example
`flock <lockfile> scripts/gate.sh`. No step of the Linux gate runs the
build-profile guard: only the ignored lanes of `regression.yml` and
`certify.yml` and the macOS job reach it. A caller may therefore set
`CARGO_PROFILE_DEV_DEBUG` and `CARGO_PROFILE_TEST_DEBUG` to `line-tables-only`
for the gate. The guard itself refuses any `CARGO_PROFILE_*` variable, an
optimization flag in `RUSTFLAGS` or `CARGO_ENCODED_RUSTFLAGS`, a profile table
in a cargo config or the workspace `Cargo.toml`, and a relative or unresolved
`CARGO_HOME`; keep those out of any shell that runs the ignored lanes.

The gate does not run the nightly lanes: `counter_gate`, the compiled registry
and inventory audits, and the `lockdown` and `kill_recovery` fault lanes run in
`regression.yml` only. A green gate is therefore the PR's Linux checks, not the
full regression. When a change touches store-open or watcher semantics, or a
statement or path a counter or memory bar measures, run the deep lanes on the
candidate:

```sh
gh workflow run regression.yml --ref <branch> -f candidate=<full sha>
```

A full local test run leaves a cargo target of roughly 40 GB in its checkout:
about 100 debug test executables of 100 to 400 MB each, plus incremental state.
Every checkout keeps its own target, so parallel full runs across checkouts can
fill a disk. Run one full gate or build at a time on a host, check free space
before starting, and delete a checkout's `target/` once its PR is pushed and
verified. An ad-hoc run that executes no measurement suite can shrink its target
with `CARGO_INCREMENTAL=0`, `CARGO_PROFILE_DEV_DEBUG=line-tables-only` and
`CARGO_PROFILE_TEST_DEBUG=line-tables-only`. A measurement suite refuses any
profile override, so a full gate runs without them.

An ad-hoc run that executes no measurement suite can also pass
`--config profile.dev.package.libsqlite3-sys.opt-level=3` to `cargo test` for
faster functional tests. The build-profile guard cannot see a `--config` flag,
so never pass it to a measurement suite.

The default macOS volume (APFS) folds case, so a walk or normalizer test with a
wrong case-sensitive branch passes on a Mac and fails only on the hosted Linux
lane. Before opening a PR that touches `norn-fs` path normalization,
shadow-name reading, exclusions or walk descent, run the `norn-fs` suite once on
a case-sensitive volume:

```sh
hdiutil create -size 512m -fs "Case-sensitive APFS" -volname nvcs /tmp/nvcs.dmg
hdiutil attach /tmp/nvcs.dmg
TMPDIR=/Volumes/nvcs cargo test -p norn-fs
hdiutil detach /Volumes/nvcs
rm /tmp/nvcs.dmg
```

The suites that build their sandbox under `CARGO_TARGET_TMPDIR` (`lockdown`,
`watch_lockdown`, `walk_memory` and part of `unreadable`) and the
`watch::observation_tests` unit tests, which build under `target/guard-scratch`,
stay on the checkout's volume. The reverse case is already provisioned: `norn-testkit`'s `volume` module mounts
a case-insensitive HFS+ RAM disk for cases stated only over a folding volume.
