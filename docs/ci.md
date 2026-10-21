# CI coverage and merge checks

Stable counts and structure belong to correctness CI. Clocks and trends belong to
the dormant soak and certification tier, under [ADR 0004](decisions/0004-two-tier-measurement-and-authored-baselines.md).

## Coverage

| Evidence | Pull requests | Main pushes |
|---|---|---|
| Linux | All five jobs: build and test, architecture, induced-failure recovery, counters, and memory | The same five jobs |
| macOS native contracts | Embedding determinism, all `norn-fs` tests, complete `obsidian_parity`, four exact host cases, and ignored `folding_cost` | Included in the complete matrix |
| Remaining macOS behavior and counters | Deferred until main | Complete feature-enabled host suite and ignored `counter_gate` |

Both platforms retain their build profiles, features, assertions, and runaway guards.
macOS uses `macos-15`, asserts ARM64, and measures `folding_cost` on a real folding volume.
Host commands enable `induced-failure`; ignored host lanes name it in `LANE_FEATURES`.

The exact host cases are:

- `churn`: `a_case_flip_converges_on_a_build_from_zero`.
- `churn`: `a_case_renamed_parent_over_a_save_converges_on_a_build_from_zero`.
- `lockdown`: `an_unreadable_document_refuses_the_heal_rather_than_quarantining_it`.
- `lockdown`: `an_unreadable_subtree_refuses_the_heal_rather_than_pruning_it`.

An exact filter runs beside its explicit Cargo test target. `test-suite.sh` rejects
a successful command whose final libtest summary reports no passing cases.
The ignored-lane runner shares that guard. Both preserve command failures and stream output.
Every correctness suite runs through the flake tripwire, which records ledger matches without retries or verdict changes.

## Triggers and main failures

`ci.yml` runs on pull requests, main pushes, and manual dispatch. Its focused Mac
job runs only on pull requests or manual dispatch. Linux jobs run on all three events.
`macos-main.yml` runs on main pushes or manual dispatch, with two independent jobs:
complete platform contracts and complete folding counters. No third Mac job runs on main pushes.

APFS and FSEvents failures outside the focused cases can first appear after merge.
A failed main sweep pauses feature merges. Repair PRs remain eligible after their required checks pass.
This is a manual merge policy; no workflow automatically freezes merges.
The policy does not require waiting for each in-flight sweep before another merge.

Only a successful sweep of current main clears the pause: all five Linux jobs and
both full Mac jobs must pass for the current main SHA. A focused Mac run, an older
green SHA, a cancelled run, or an incomplete sweep cannot clear it.
A diagnosed runner failure follows the read-before-rerun rule; diagnosis alone does not clear the pause.

Main runs do not cancel running predecessors. GitHub can replace pending runs, so
coverage promises eventual testing of current main, not every intermediate SHA.
Results establish only their tested SHA. A manual run on another ref cannot establish current-main coverage.
The soak and certification campaign remains dormant.

## Required pull-request contexts

The expected required contexts are:

- `build and test`
- `architecture gate`
- `induced-failure recovery`
- `counter gates`
- `memory invariant`
- `determinism and filesystem (macOS ARM64)`
- `CodeRabbit`

The native context retains its name and reports the focused contracts above.
`counter bars on a folding root (macOS ARM64)` belongs to the main sweep and is not a PR requirement.
The live ruleset transition must remove that obsolete PR requirement when this split is adopted.
That policy update is coordinated separately with the workflow PR. A missing, skipped, or no-op check cannot substitute for retired coverage.
