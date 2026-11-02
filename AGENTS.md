# norn

## Architecture

`docs/architecture.md` describes the running system and is the authoritative form of the invariant spine and the crate map — the contract reviews enforce against, including the dependency allowlist and the boundary invariants. Load it before proposing structural changes, adding a crate, or adding a dependency edge.

## ADRs and Glossary

We use the `documentation` skill for recording important decisions as well as glossary items. These can be found in the `docs/glossary.md` file and the `docs/decisions/` directory. An index of all decisions is maintained in `docs/decisions/README.md`, load this to get a high-level overview. These are load-bearing and should not be violated without a discussion with the user.

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
- **Keep cargo dependencies current.** Cargo dependencies move by hand: `.github/dependabot.yml` tracks only GitHub Actions. When a bump needs a small code migration, make the migration with the bump rather than letting breaking-API bumps pile up. Run the architecture gate (`cargo test -p norn-testkit --test architecture`) on every bump: a workspace edge must be in the dependency allowlist in `docs/architecture.md`, and a crate declared isolated must still link nothing under the default selection.
- **On a user-visible behavior fork, the least surprising answer wins.** For ordering, case handling and matching semantics, do what Obsidian, other mainstream Markdown vault tools and file browsers already do. Check them first and name what you checked in the recommendation. ADR 0040 rules this for links. The same posture settled that tags compare under a case fold (the tag fold in `norn-config`) and that paths answer in case-folded order, `(path COLLATE NOCASE, path)`, whether or not the root folds.
- **A brief transient window takes the least-effort answer.** When a question concerns only a short transient state, such as the seconds between a schema change and the re-derivation of rows, choose the least-effort option and state the accepted window in an inline comment. Do not add machinery for correctness inside the window. This does not extend to steady-state correctness, or to a read that blocks another read.

## Performance posture

- **Performance is a design goal.** This rewrite exists to beat the legacy line. A quadratic or seconds-long worst case on a real path is a defect, not a declared limit. Look for a formulation whose cost follows the documents or links the work touches, measure release-build timings early, and pair every clock with a logical counter. Offer a quadratic worst case only as a clearly flagged non-default option.
- **Warm operations come first.** Reads and writes on an attached vault matter far more than a from-zero build reaching Ready. A cold-path slowdown is acceptable when it buys correctness or enables later work. Only human-scale changes merit investigation: a 100 ms operation growing toward a second, or a per-item cost that multiplies to a quarter of a second or more. Microsecond-level warm deltas are not worth chasing.
- **A measured human-scale loss is a defect to diagnose, not a trade-off to recommend.** A first Get that grows from 0.7 s to 9.6 s is an example. Compare against the best known number: current main, and the legacy line's full build of about 3.8 s at 10,000 documents (`docs/performance.md` runs legacy Norn 0.48.3 beside main). A change to when or what a user is answered is a product decision for the maintainer, even in the middle of a prototype.
- **No operator messaging before the CLI exists.** Progress text and "still working" signals wait for the CLI. Keep the data such messages need available for the CLI to consume.

## Public material

- norn is a generic Markdown vault tool. Committed and public material (docs, examples, fixtures, skill templates, package text, commit messages and PR text) never names a maintainer's private vault or its notes. Use generic field and folder names. Public agent harnesses, such as Claude Code, Codex, OpenCode, OpenClaw, Hermes and Pi, may be named as example consumers.
- The ADR corpus is self-contained. An ADR carries no work-tracker id (`NORN-*`, `NRN-*`) and names no consumer project ("a consumer project" instead). A private design note appears as an italic title, never as a link. Work-tracker ids are allowed outside the ADR corpus, as in this file and `docs/architecture.md`.

## Pre-release posture

`norn` is pre-1.0 with no known or supported external consumers yet. This is the window for breaking changes without coordinating upgrades — churn is cheap until 1.0. Regardless, though we don't avoid breaking changes at this stage, we cause them thoughtfully and discuss them before hand.
