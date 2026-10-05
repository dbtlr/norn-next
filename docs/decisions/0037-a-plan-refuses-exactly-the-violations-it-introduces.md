---
status: accepted
date: 2026-10-05
---

# 0037 — a plan resolves into per-file transitions whose states say whether their bytes are a document, every target is checked and staged before any is published, a plan refuses exactly the violations it introduces under the schema it lands under, and re-applying a resolved plan finishes it

Supersedes [ADR 0032](0032-a-file-state-says-whether-its-bytes-are-a-document.md), whose
contract this decision restates whole, as ADR 0032 restated [ADR
0031](0031-a-plan-is-staged-whole-and-finished-by-reapplying.md) with one refinement: a
present file state records, beside its hash, whether its bytes decode as a vault document,
because a plan's link conditions count only documents as link targets and the applier no
longer holds a landed target's before-bytes. ADR 0032 also counted the findings a repair
acts on among a plan's conditions, and refused every violation on a field a plan writes.
Repair compiles findings into resolved plans under schema rules that combine per field and
judge a list element by element ([ADR
0035](0035-a-schema-rule-selects-documents-by-their-frontmatter.md), [ADR
0036](0036-a-repair-fix-is-declared-on-the-constraint-it-serves.md)), and neither clause
holds up there. No change can move a finding a plan fixes without changing a target's bytes,
a link resolution, a document a fix read, or the schema, and each of those has its own
guard, so a findings condition adds nothing and was never carried on the wire. And a repair
that rewrites one element of a list writes the field, so the field-level rule would refuse
it over every element it leaves standing. **No finding is a condition: a repair plan cites
the finding generation it read as provenance only. Evidence a repair fix read from another
document is guarded by a content-hash condition on that document, and a fix that writes a
link records a written-link condition on the address it writes and a replaced-address
condition on any address it replaces, a condition kind of its own carried beside the link
resolutions the plan's operations change. A plan refuses exactly the violation identities it
introduces — a violation's identity being its kind, field, offending value and combined
constraint — and a previewed plan applies where its results pass that check under the schema
they land under.** Everything else ADR 0032 ruled stands as it was.

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
  before the write, and the state it holds after; each state is either absent or present
  with a content hash and whether those bytes decode as a vault document. Operations on one file compose in order at planning time and stand or fall
  together. An operation over a set of documents — a `set` over a query, a folder move —
  expands at planning time into one operation per document. A move's destination must be
  absent at planning, or vacated by another operation of the same plan, which the move then
  requires; on a root that folds case, a destination that differs from its source only in
  case names the source itself, and a case-only rename is that move, not an occupied
  name.
- **A file state says whether its bytes are a document.** Whether bytes decode is a
  function of the bytes alone, by the one rule the derivation quarantines by, so the
  planner records it from the bytes it reads and composes, and every present state in a
  plan that shares a hash carries the same answer. The applier checks the record against
  every byte it holds for that hash; a record the bytes contradict is a plan whose
  transitions disagree with its operations, answered as invalid. Only where no held bytes
  pin a hash — a landed removal's or replacement's before-bytes — does the applier read the
  record as given. A link condition counts a target as a candidate on a side only where
  that side is a document, so a quarantined file a plan removes, moves or repairs changes
  exactly the resolutions it changes, at planning and at the applier alike. The record is
  written only for bytes that do not decode, so a plan holding only documents reads as
  before.
- **Everything a plan varies on is fixed at planning time.** Every template value — a
  `{{seq}}` identifier, a timestamp, a default — resolves into the resolved plan's
  operations and paths, so recomposing a target is a pure function of the before-states
  and the operations. The applier recomposes each target and refuses unless the result
  hashes to the after-state. A resolved plan carries its operations, authored content
  included, and one fixed-size record per target, never the bytes of a file it did not
  author.
- **No publication destroys content a later publication needs.** Where one target's
  content is drawn from another target's before-state — a move, or a chain of moves — the
  source is not replaced or removed until every other target drawing on it has durably
  landed, its directory and every folder the plan made for it synced. A plan
  whose content dependencies form a cycle, such as two documents exchanging places, is
  refused at planning; the caller splits it into plans that each finish.
- **Conditions the planning read travel with the plan.** A plan whose effect depends on
  facts it does not write — the documents a link target resolves to, the backlinks a removal
  would break — carries them as conditions: content hashes, and the link resolutions its
  operations change. **No finding is a condition**: a repair plan cites the finding
  generation it read as provenance, which the applier never checks. Evidence a repair fix
  read from a document the plan does not write — the heading a missing anchor is fixed to,
  the content a followed move was matched by — travels as a content-hash condition on that
  document. A repair fix that writes a link records a **written-link condition**, the
  resolution its written address has at the plan's after-state, and one that rewrites a link
  also records a **replaced-address condition**, the resolution the replaced address has at
  the after-state, none for a broken link; so a written target that disappears or gains a
  competitor, or a broken link's old target that reappears, refuses the plan. These are a
  condition kind of their own, carried beside the link resolutions the plan's operations
  change and never among them: the applier recomputes those resolutions from the operations
  alone and takes every one the plan carries as a member of that set, which holds no entry
  for an address the after-state drops. Only repair's planner records them, so an ordinary
  write of a link value carries none. An author's condition on a file the plan writes is
  checked at planning and becomes that target's before-state. Planning records each
  condition as the vault would stand with every target of the plan at its after-state, and
  the applier checks it the same way, landed or not, after taking in the filesystem facts
  the watcher has delivered; so a plan's own progress never changes a condition, and only a
  change to a file outside the plan's targets refuses one. A foreign change the watcher has
  not yet reported is outside what a condition can see; a link it breaks surfaces as a
  link-health finding.
- **A request states its mode.** A request either previews, answering with the resolved
  plan and its forecast and writing nothing, or applies. There is no default mode at the
  wire.
- **An apply accepts operations or a resolved plan.** Operations are planned and applied in
  one request, planned inside the registration's apply serialization, and carry no confirmation beyond
  their own conditions: re-sending them is a new change. A caller that needs a write it can
  safely re-send previews and applies the resolved plan, or gives its operations
  conditions, so a re-send refuses rather than repeats. A resolved plan is applied only
  when the vault's root identity matches, its conditions hold, and every target holds its
  before-state or its after-state.
- **Nothing is published until everything is checked and staged.** Before the first
  publication the applier checks every target's state and every condition, validates every
  composed result against the vault schema, and stages every written target — a created
  document included — as a shadow. A refusal found in this phase publishes nothing and
  answers as refuse-and-refresh below. No handle is held from staging to
  publication: each publication verifies its target again.
- **Publication order and what can still stop it.** Creates publish first, each by an
  exclusive, atomic publication that never replaces a name, with any parent folder it
  needs made just before it is published; replacements follow; removals come last, and a folder the
  plan's removals leave empty is removed after them, on a re-send too. A folder made for a
  create that then refuses is removed if it is empty; one a crash leaves before its create
  lands stays empty until a re-send publishes into it or removes it. Folders are not transitions, and the
  forecast and the report name them. Four things can stop publication part-way: a crash;
  an I/O failure; a create whose name another writer took after staging; and a foreign
  edit reaching a target after its staging, which that target's verification refuses. A
  foreign write landing between a target's verification and its rename is overwritten:
  this is the per-file protocol's stated residual race, and the watcher converges the
  store on what the path holds. An attempt that stops before any target lands has written
  no document, only possibly an empty folder: a refusal when a check stopped it, a failure
  when I/O did. An attempt that stops
  after at least one of its targets landed is **interrupted**, and its report names every
  target that landed.
- **A target at its after-state is landed.** A removal is landed when its path is absent. A
  move is landed when its destination holds its after-state and its source is absent, each
  leg recognized on its own; since a source is removed only after its destination durably
  landed, a source found absent while its destination is not at its after-state was removed
  by another writer, which is drift, and the move is unresolved. A target at its after-state is landed whichever writer put it
  there. Re-applying a resolved plan treats a landed target as done, so re-sending a plan
  a crash or an I/O failure interrupted finishes it. A resolved plan whose targets return
  to their before-states applies again.
- **Refuse-and-refresh.** A target at neither state is drift: the apply refuses, naming each
  drifted target, and answers with a fresh resolved plan and its forecast. The fresh plan
  drops every operation whose targets all hold their after-states, and re-resolves against
  what the vault now holds only an operation none of whose targets holds its after-state.
  An operation with a target at its after-state and another that is not, an operation
  that no longer resolves (an edit whose anchor is gone, a move whose destination is no
  longer absent), and an operation that requires an unresolved operation, directly or
  through others, are listed as unresolved, never re-resolved or dropped; the caller
  disposes of them. Hashes cannot tell a target edited after this plan landed on it from one edited
  before, and the host keeps no record of an earlier attempt, so the forecast marks every
  drifted target as possibly already carrying this plan's change. Applying the fresh plan
  is the caller's decision. Auto-rebase stays rejected.
- **A plan refuses exactly the schema violations it introduces.** A violation's identity is
  its kind, the field it stands on, the offending value it names and the combined constraint
  it breaches ([ADR 0035](0035-a-schema-rule-selects-documents-by-their-frontmatter.md)); a
  violation about the whole document names no field and no value, and an undeclared tag's
  identity is its kind and the tag, wherever the document writes it, in frontmatter or body.
  A violation whose identity did not stand before the plan refuses. A standing violation
  whose identity is unchanged does not, even on a field the plan writes or for a tag the
  plan writes a different number of times, and an unrelated violation already present in a
  target does not either. The check runs under the vault schema pinned when the plan
  applies: a resolved plan applies where its composed results introduce no violation under
  the schema they land under, a violation that schema finds already standing included, and
  refuses where they introduce one. A plan that changes a vault control file changes nothing
  else.
- **A plan names its vault by address and carries the vault's root identity.** Applied to a
  vault whose root identity differs, it refuses.
- **Applies serialize per registration; one uninterrupted apply is one changeset.** An
  apply runs as a job holding its entry's claim, so applies through one registration run
  one at a time and no derivation job interleaves with one. Other registrations over the
  same root are ordinary concurrent writers ([ADR 0016](0016-maintainer-singleton-per-derived-store.md)),
  held apart by per-file compare-and-swap and create exclusivity, and each derives the
  applier's writes as it sees them. An uninterrupted apply commits one changeset to its
  registration's store, so a read there sees the whole state before it or the whole state
  after it. An interrupted apply commits exactly the targets that landed; after a crash,
  or when the changeset itself cannot commit, the entry heals from what the paths hold and
  the report names what landed. The caller's request waits for the outcome; a caller that
  stops waiting does not abort the apply.

## Considered options

- **Every write in two requests** — a verb returns a plan and nothing is written until the
  caller sends it back. Rejected: it doubles the requests for the most common change, and a
  preview is available to any caller that wants one.
- **Plans or idempotency keys held by the host** — a verb returns an identifier the caller
  confirms or retries by. Rejected: host state needs expiry, is lost on restart, and serves
  only a caller that reaches the host holding it. A resolved plan is already the retry
  token, carried by the caller.
- **A resolved plan carrying the composed content of every target** — rejected: a
  vault-wide plan would hold the bytes of every file it rewrites, which the memory
  invariant forbids, and an edited plan could publish content no operation produced.
- **Stop at the first refusal and report** — publish target by target and report what
  landed. Rejected: an ordinary foreign edit to one target leaves a migration half-applied,
  which is the failure the previous line's applier had.
- **Rollback** — keep each target's previous content durably and undo on failure.
  Rejected: an undo across files can itself fail part-way, and the previous contents become
  durable state to heal. Checking and staging everything first narrows what can stop
  publication to the four cases above; re-applying covers a crash or an I/O failure with no
  state beyond the plan, and the others are drift the caller re-plans, as anywhere else.

- **Refuse to plan a removal, move or edit of a quarantined file** — every target would
  then be a document and a hash would suffice. Rejected: removing, moving and repairing a
  file norn cannot decode is exactly what a person or a Layer 5 repair needs to do with it.
- **Read whether a target was a document from the derived store** — no wire change.
  Rejected: once the store has taken in a landed removal, a removed document and a removed
  quarantined file leave the same absence, and an edit that makes undecodable bytes decode
  changes document-ness without changing presence, so the condition would be inexact in
  both directions.

- **A findings condition** — a repair plan records the findings it acts on, and the applier
  refuses where they changed. Rejected: every change that could move a finding a plan fixes
  in a target changes that target's bytes, which its before-state refuses; what a fix rests
  on elsewhere is a link resolution or another document's content, which conditions already
  record; and re-judgment refuses a result the landing schema rejects. A condition on the
  finding generation would refuse a plan over every unrelated derivation.
- **A schema-fingerprint condition** — refuse a plan previewed under another schema.
  Rejected: a reload that leaves the plan's results passing the write check would refuse it
  for nothing, and the write check under the schema the plan lands under is the exact test.
- **Keep the field-level written-field rule** — refuse every standing violation on a field
  the plan writes, and every standing undeclared tag whose count the plan changes. Rejected:
  a repair fixing one element of a list writes the field and would refuse over every
  element it leaves standing, so a list could be repaired only whole; and a write that
  repeats or drops one occurrence of a tag would refuse over a tag it did not introduce.

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
- A file state's wire form gains one field written only when true, so every plan whose
  targets are all documents serializes as it did under ADR 0031.
- Layer 5's repair emits the same plans, carrying the findings it skipped and the finding
  generation it read, and the same applier executes them.
- What the caller previewed is what applies, and whether it may is judged under the schema
  it lands under: a reload between a preview and its apply refuses the plan only where its
  results introduce a violation under the new schema.
- Where a fix's uniqueness rests on another document's absence — a second document holding
  the content a followed move was matched by, a closer match for a suggestion — that
  absence is judged at planning and is not a condition: a second holder or a closer match
  appearing after the preview does not refuse the plan. A written link target that gains a
  competing resolution does refuse it, through its written-link condition.
