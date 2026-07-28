# norn

## Architecture

`docs/architecture.md` describes the running system and is the authoritative form of the invariant spine and the crate map — the contract reviews enforce against, including the dependency allowlist and the boundary invariants. Load it before proposing structural changes, adding a crate, or adding a dependency edge.

## ADRs and Glossary

We use the `domain-modeling` skill for recording important decisions as well as glossary items. These can be found in the `docs/glossary.md` file and the `docs/decisions/` directory. An index of all decisions is maintained in `docs/decisions/README.md`, load this to get a high-level overview. These are load-bearing and should not be violated without a discussion with the user.

## How we work

- **No broken windows.** If you find a bug or defect, even if you didn't cause it, it is now *your* responsibility to either fix it or file it. Work with the user to understand which is the right choice.
- **Never push to main.** All work should be done in a branch or worktree and pushed as a PR. The PR is the user review gate
- **Small meaningful commits.** Create useful checkpoints on long tasks, that indicate a working point in time.
- **Not complete until it is verified.** All tasks should have a verification run by an adversarial agent and be presented to a human as a PR before a task is filed as completed. The size and composition of the adversatial review is determined by the type, size, complexity, and impact of the change.

## Pre-release posture

`norn` is pre-1.0 with no known or supported external consumers yet. This is the window for breaking changes without coordinating upgrades — churn is cheap until 1.0. Regardless, though we don't avoid breaking changes at this stage, we cause them thoughtfully and discuss them before hand.
