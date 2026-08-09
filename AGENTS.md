# norn

## Architecture

`docs/architecture.md` describes the running system and is the authoritative form of the invariant spine and the crate map — the contract reviews enforce against, including the dependency allowlist and the boundary invariants. Load it before proposing structural changes, adding a crate, or adding a dependency edge.

## ADRs and Glossary

We use the `domain-modeling` skill for recording important decisions as well as glossary items. These can be found in the `docs/glossary.md` file and the `docs/decisions/` directory. An index of all decisions is maintained in `docs/decisions/README.md`, load this to get a high-level overview. These are load-bearing and should not be violated without a discussion with the user.

## Durable records

- Task annotations own task-specific implementation choices, completion evidence,
  measurements, limitations, and follow-ups. Session logs own the broader work
  narrative. Mechanism-specific contracts live beside their code, tests, or
  workflows; `docs/architecture.md` owns the current cross-cutting system contract.
- Create an ADR only when a decision is hard to reverse, surprising without its
  rationale, and the result of a real trade-off. Write it when the decision is
  made, before implementation or immediately when it crystallizes—not afterward
  to justify task completion.
- Accepted ADR content is immutable. A superseded ADR changes only its status and
  link to the newly authoritative ADR.
- Add a glossary term only for project-specific language future tasks must use
  consistently. Keep definitions implementation-free and organize terms by
  concept, never by the task or ADR that introduced them.
- One ADR or glossary section landing with each task is a review smell. Stop and
  verify that the task actually surfaced a durable decision or reusable term.

## Dormant carriers

- Default-EXCLUDE excludes legacy doctrine, not planned end-state needs. Before
  calling an unreached path dead, test it against the layer roadmap: a path with
  a named consuming layer is a **dormant carrier**; a path without one is
  speculative and remains excluded.
- Keep a dormant carrier covered at its seam. Its inline documentation must name
  the consuming layer and explain why the current call graph does not yet reach
  it, so implementation and review briefs preserve that context.
- A dormant carrier may be removed only by a new ruling that withdraws its
  roadmap obligation or establishes a different carrier for it; an empty call
  graph is not such a ruling. In a NORN-bound work session, consult Mimir
  artifact NORN-a73 for carriers removed before this doctrine was explicit;
  its source commits are re-derivation templates, not reversions to replay.

## How we work

- **No broken windows.** If you find a bug or defect, even if you didn't cause it, it is now *your* responsibility to either fix it or file it. Work with the user to understand which is the right choice.
- **Never push to main.** All work should be done in a branch or worktree and pushed as a PR. The PR is the user review gate
- **Small meaningful commits.** Create useful checkpoints on long tasks, that indicate a working point in time.
- **Not complete until it is verified.** All tasks should have a verification run by an adversarial agent and be presented to a human as a PR before a task is filed as completed. The size and composition of the adversatial review is determined by the type, size, complexity, and impact of the change.

## Pre-release posture

`norn` is pre-1.0 with no known or supported external consumers yet. This is the window for breaking changes without coordinating upgrades — churn is cheap until 1.0. Regardless, though we don't avoid breaking changes at this stage, we cause them thoughtfully and discuss them before hand.
