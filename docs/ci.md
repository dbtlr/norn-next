# CI coverage and regression evidence

Ordinary functional tests remain required on pull requests, including doctests and
feature-enabled host cases. Stable counts and structure are correctness evidence.
Clocks and trends belong to the dormant soak tier under
[ADR 0004](decisions/0004-two-tier-measurement-and-authored-baselines.md).

## PR and full-regression coverage

`ci.yml` runs six jobs on PRs, main pushes and manual runs. Ordinary default
workspace tests and doctests split by package between `build and test` (everything
except `norn-host` and `norn-testkit`) and
`build and test (norn-host and norn-testkit)`. The named shard uses `-p norn-host -p norn-testkit`; the catch-all uses `--workspace --exclude norn-host --exclude norn-testkit`.
Their package lists must match exactly, so new crates join the catch-all and
every package's doctests run exactly once through ordinary `cargo test`.
The shard pair preserves whole-workspace test membership, and each shard builds
tests with the whole-workspace feature set. The architecture job enforces both
claims with `cargo test -p norn-testkit --test shards`: it reads each shard's
selection from `ci.yml`, resolves features with `cargo tree` (normal,
development and build edges, host target) for the whole workspace and for each
shard, and fails naming the shard, package and feature when a shard builds any package
in its graph, selected root or dependency, with a different feature set, or when the shards do not partition the workspace.
The whole feature-enabled
`norn-host` package runs in `host functional (induced-failure)`, so current and
future ordinary targets and cases join PR coverage automatically.

| PR job | Evidence |
|---|---|
| Build and test | Workspace build, default tests and doctests except norn-host and norn-testkit, cheap registry structure and dormancy checks |
| Build and test (norn-host and norn-testkit) | Default norn-host and norn-testkit tests and doctests; cargo test builds its selected targets |
| Lint and document | Format, all five clippy contexts, performance-harness unit tests, denied-warning rustdoc |
| Host functional (induced-failure) | Complete ordinary feature-enabled host suite, including churn and rolling-heal correctness and work counters |
| macOS platform | Embedding determinism, filesystem contracts, Obsidian parity, exact case-rename and unreadable-document/subtree cases, folding-root spelling bar |
| Architecture | Actionlint, the architecture gate and the shard certification |

`regression.yml` runs the complete seven-job matrix at 02:17 UTC daily and by
manual dispatch. Its `full ` job names retain the build, induced-failure,
platform, folding-counter, architecture, Linux counter and Linux memory jobs.
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

- The same run attempt completed successfully with all seven expected full jobs.
  Their names are the complete matrix names prefixed with `full `.
- Every required job and step executed its obligations successfully. Missing,
  skipped or cancelled required evidence does not qualify.
- All seven receipts identify their expected job and the same run ID and attempt.
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

A full local test run leaves a cargo target of roughly 40 GB in its checkout:
about 100 debug test executables of 100 to 400 MB each, plus incremental state.
Every checkout keeps its own target, so parallel full runs across checkouts can
fill a disk. Run one full gate or build at a time on a host, check free space
before starting, and delete a checkout's `target/` once its PR is pushed and
verified. An ad-hoc run that executes no measurement suite can shrink its target
with `CARGO_INCREMENTAL=0`, `CARGO_PROFILE_DEV_DEBUG=line-tables-only` and
`CARGO_PROFILE_TEST_DEBUG=line-tables-only`. A measurement suite refuses any
profile override, so a full gate runs without them.

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
