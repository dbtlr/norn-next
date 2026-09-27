---
status: accepted
date: 2026-09-27
---

# 0031 — a plan is a self-contained value, every target is staged before any is published, and re-applying a plan finishes it

A plan that touches many documents cannot be published atomically: the filesystem offers
one atomic publication per name, and `norn-fs`'s write protocol makes each of those safe on
its own. The question is what a multi-document plan promises when one of its targets
cannot be written. **Every target of a plan is checked and staged before any target is
published, so a refusal writes nothing; a plan records the state each target must hold
after it lands, so re-applying a plan finishes what an interruption left; and a plan is a
self-contained value, so the host holds no plan between requests.** The same contract
serves a one-field change and a vault-wide migration: a write verb is a plan of one
operation, and both run through the one planner and the one applier invariant 4 names.

## The contract

- **Operations in, a resolved plan out.** A caller hands the host a list of operations —
  each a kind, its fields, an optional identifier, and optional dependencies on other
  operations. The planner resolves the list into one transition per file it touches: the
  content hash the file must hold before the write, the composed content, and the content
  hash that content has. Operations on one file compose in order at planning time. The
  resolved plan is the forecast.
- **A request states its mode.** A request either previews, returning the resolved plan and
  writing nothing, or applies. There is no default mode at the wire.
- **An apply accepts either form.** An operation list is planned and applied in one request.
  A resolved plan is applied only if every target still holds its before-hash.
- **Nothing is published until everything is staged.** Before the first publication the
  applier checks every target's precondition, validates every composed result against the
  vault schema, and stages every shadow. A refusal at any target publishes nothing and
  answers with a fresh forecast. Publication is a window, not an act: a crash, an I/O
  failure, or a foreign edit reaching a target between its staging and its swap can still
  stop it part-way, and the apply report names every target that landed.
- **A target at its after-hash is landed.** Re-applying a plan treats a target that already
  holds its planned content as done rather than as drift, so re-sending a plan a crash or
  an I/O failure interrupted finishes it. A target a foreign edit reached refuses as drift,
  as it would before publication, and is re-planned.
- **A plan refuses only the schema violations it introduces.** A violation already present
  in a target, and unrelated to the plan, does not refuse it.
- **A plan names its vault by address, never by a local path.** Any caller the host serves
  may author or relay one.
- **One apply at a time per vault, one changeset per plan.** Applies to one vault run in
  order. A plan's write-through commits as one store changeset, so a read sees the whole
  state before the plan or the whole state after it. The caller's request waits for the
  outcome; a caller that stops waiting does not abort the apply, and re-sending the plan
  reports what landed.

A move remains two operations on the filesystem, ordered so that an interruption leaves
both names holding the document and never neither.

## Considered options

- **Every write in two requests** — a verb returns a plan and nothing is written until the
  caller sends it back. Rejected: it doubles the requests for the most common change, and a
  preview is available to any caller that wants one.
- **Plans held by the host** — a verb returns a plan identifier the caller confirms.
  Rejected: plan state in the host needs expiry, is lost on restart, and serves only the
  caller that can reach the host that holds it.
- **Stop at the first refusal and report** — publish target by target and report what
  landed. Rejected: an ordinary foreign edit to one target leaves a migration half-applied,
  which is the failure the previous line's applier had.
- **Rollback** — keep each target's previous content durably and undo on failure.
  Rejected: an undo across files can itself fail part-way, and the previous contents become
  durable state to heal. Staging everything first narrows what can interrupt publication to
  a crash, an I/O failure, or a foreign edit inside the publication window; re-applying
  covers the first two with no state beyond the plan, and the third is drift the caller
  re-plans, as it is anywhere else.

## Consequences

- The `norn-fs` write protocol splits into a staging step and a publishing step; a
  single-file write is one of each.
- A large plan holds a staged shadow per target until it publishes.
- The obligation that a commit consume the snapshot its verb read is met by the hashes a
  plan carries, which the applier checks against each file, rather than by the reader's
  connection.
- Layer 5's repair emits the same plans, carrying the findings it skipped, and the same
  applier executes them.
