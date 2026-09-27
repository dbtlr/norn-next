---
status: accepted
date: 2026-09-27
---

# 0031 — a plan resolves into per-file transitions, every target is checked and staged before any is published, and re-applying a resolved plan finishes it

A plan that touches many documents cannot be published atomically: the filesystem offers
one atomic publication per name, and `norn-fs`'s write protocol makes each of those safe on
its own. The question is what a multi-document plan promises when one of its targets
cannot be written. **Every target of a plan is checked and staged before any target is
published, so a refusal found before publication writes nothing; a resolved plan records
the state each target holds before and after it, so re-applying a resolved plan finishes
what a crash or an I/O failure interrupted; and a plan is a self-contained value, so the
host holds no plan between requests.** The same contract serves a one-field change and a
vault-wide migration: a write verb is a plan of one operation, and both run through the
one planner and the one applier invariant 4 names.

## The contract

- **Operations and transitions.** A caller authors a plan as operations: each a kind, its
  fields, an optional identifier, optional dependencies on other operations, and optional
  conditions its author observed — a content hash, an expected value, or an edit anchor.
  The planner resolves the operations into a **resolved plan**: the operations, one
  **transition** per file they touch, the conditions the planning read and does not write,
  and the vault's root identity. A transition names a path, the state the file must hold
  before the write, and the state it holds after; each state is either absent or a content
  hash. Operations on one file compose in order at planning time. Template allocations,
  such as a `{{seq}}` identifier, resolve at planning time into a transition's path.
- **A resolved plan carries states, not content.** The applier recomposes each target from
  its before-state and the plan's operations, and refuses unless the result hashes to the
  after-state. A plan's size is a function of the targets it touches, never of their bytes.
- **Conditions the planning read travel with the plan.** A plan whose effect depends on
  facts it does not write — the documents a link target resolves to, the backlinks a
  removal would break, the finding generation a repair was planned against — carries them
  as conditions, and the applier checks them against the store at apply time.
- **A request states its mode.** A request either previews, answering with the resolved
  plan and its forecast and writing nothing, or applies. There is no default mode at the
  wire.
- **An apply accepts operations or a resolved plan.** Operations are planned and applied in
  one request, planned inside the vault's apply serialization, and carry no confirmation
  beyond their own conditions: re-sending them is a new change. A caller that needs a
  write it can safely re-send previews and applies the resolved plan, or gives its
  operations conditions, so a re-send refuses rather than repeats. A resolved plan is
  applied only when the vault's root identity matches, its conditions hold, and every
  target holds its before-state or its after-state.
- **Nothing is published until everything is checked and staged.** Before the first
  publication the applier checks every target's state and every condition, validates every
  composed result against the vault schema, and stages every written target — a created
  document included — as a shadow. A refusal found in this phase publishes nothing. No
  handle is held from staging to publication: each publication verifies the target again.
- **Publication order and what can still stop it.** Creates publish first, each by an
  exclusive, atomic publication that never replaces a name; replacements follow; removals
  come last. Publication is a window, and four things can stop it part-way: a crash; an
  I/O failure; a create whose name another writer took after staging, since exclusivity is
  decided at publication and creates going first means such a refusal leaves only other
  new documents; and a foreign edit reaching a target after its staging, which that
  target's verification refuses. A foreign write landing between a target's verification
  and its rename is overwritten: this is the per-file protocol's stated residual race, and
  the watcher converges the store on what the path holds. An apply that stops part-way is
  reported as interrupted, naming every target that landed; it is not a refusal.
- **A target at its after-state is landed.** A removal is landed when its path is absent. A
  move is landed when its destination holds its after-state and its source is absent, each
  leg recognized on its own. Re-applying a resolved plan treats a landed target as done
  rather than as drift, so re-sending a plan a crash or an I/O failure interrupted finishes
  it. A target a foreign edit reached refuses as drift and is re-planned. A resolved plan
  whose targets return to their before-states applies again.
- **A plan refuses the schema violations it introduces.** A violation on a field the plan
  writes, or one that did not stand before the plan, refuses; an unrelated violation
  already present in a target does not. A plan that changes a vault control file changes
  nothing else.
- **A plan names its vault by address and carries the vault's root identity.** Applied to a
  vault whose root identity differs, it refuses.
- **Applies serialize per registration; one uninterrupted apply is one changeset.** An
  apply runs as a job holding its entry's claim, so applies through one registration run
  one at a time and no derivation job interleaves with one. Other registrations over the
  same root are ordinary concurrent writers ([ADR 0016](0016-maintainer-singleton-per-derived-store.md)),
  held apart by per-file compare-and-swap and create exclusivity. An uninterrupted apply
  commits one store changeset, so a read sees the whole state before it or the whole state
  after it. An apply that stops part-way commits exactly the targets that landed; after a
  crash, the heal derives what the paths hold. The caller's request waits for the outcome;
  a caller that stops waiting does not abort the apply.

## Considered options

- **Every write in two requests** — a verb returns a plan and nothing is written until the
  caller sends it back. Rejected: it doubles the requests for the most common change, and a
  preview is available to any caller that wants one.
- **Plans or idempotency keys held by the host** — a verb returns an identifier the caller
  confirms or retries by. Rejected: host state needs expiry, is lost on restart, and serves
  only a caller that reaches the host holding it. A resolved plan is already the retry
  token, carried by the caller.
- **A resolved plan carrying composed content** — rejected: a vault-wide plan would hold
  the bytes of every target, which the memory invariant forbids, and would let an edited
  plan publish content no operation produced.
- **Stop at the first refusal and report** — publish target by target and report what
  landed. Rejected: an ordinary foreign edit to one target leaves a migration half-applied,
  which is the failure the previous line's applier had.
- **Rollback** — keep each target's previous content durably and undo on failure.
  Rejected: an undo across files can itself fail part-way, and the previous contents become
  durable state to heal. Checking and staging everything first narrows what can stop
  publication to the four cases above; re-applying covers a crash or an I/O failure with no
  state beyond the plan, and the others are drift the caller re-plans, as anywhere else.

## Consequences

- The `norn-fs` write protocol splits into a staging phase and a publishing phase. A create
  gains a shadow and an exclusive publication that never replaces a name; a removal and a
  move's source are checked while staging and removed while publishing.
- A plan holds one shadow per written target on disk until it publishes, and no open
  handle between the phases.
- The own-write ledger's lifetime and capacity bound how long and how large an apply can be
  before its watcher echoes are derived again after its changeset: redundant work, never a
  wrong answer.
- Preconditions still come from planning time: for a resolved plan, what the caller
  previewed is what applies; for operations, the conditions the caller wrote are the
  confirmation. The obligation that a commit consume the snapshot its verb read is met by
  the states and conditions a plan carries rather than by the reader's connection.
- Layer 5's repair emits the same plans, carrying the findings it skipped and the finding
  generation it read, and the same applier executes them.
