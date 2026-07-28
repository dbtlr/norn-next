# 0001 — Restart decision of record

## Status

Accepted (2026-07-27). Founding decision of this line; every later ADR is numbered
against it.

Source of record: Mimir artifact **NORN-a1**, a frozen set of eighteen ratified rulings.
(*Mimir* is the work-state system this repository binds to via `.mimir.toml`; every
`NORN-*` and `NRN-*` identifier in this document is a Mimir task, artifact, or seed ID and
resolves there.)

NORN-a1 is the archived record; this ADR is the present-tense contract the repository is
built and reviewed against.

- Where the two differ in **wording**, this ADR governs.
- A **substantive** divergence from a ruling is not settled by this document; it requires
  its own recorded ruling, and this ADR is then amended to match.
- Where this ADR is **silent**, NORN-a1 is the evidence to consult.

Companion documents: [`docs/architecture.md`](../architecture.md) holds the invariant spine
and the crate map; the [decisions index](README.md) lists every ADR; the
[glossary](../glossary.md) holds domain terms as the layers that earn them land.

## Context

`norn` is a Markdown-vault engine. A previous line of the product exists, is released,
and is preserved read-only as **evidence**: a repository (`norn-legacy`), a work board
(`NRN`), and a workspace of design notes. That line reached a state where two defects
were structural rather than incidental:

1. **Wrong build order.** Top-down porting under an output-comparison harness carried
   the prior internal architecture along with the outputs it reproduced. The shape
   arrived before the contract that would have justified it.
2. **Absent enforcement.** Plan guards were unproven, instrumentation was absent, and
   the mission property — cost proportional to the *changed* set, not the vault size —
   had no property test anywhere.

The audit that established those defects — the reckoning appendices, Mimir **NRN-a59**
through **NRN-a63** — also vindicated the core design: the shipped schema answers 13 of 14
read gaps in 0.00–3.7 ms. The problem is the build order and the missing enforcement, not
the idea.

This ADR records the response: a blank-repo, bottom-up restart whose first layer is the
enforcement machinery itself.

## Standing laws

Every ruling below is subordinate to these. They apply to all layers, all reviews, and
all imports.

- **Blank repository.** The tree starts empty.
- **Default-EXCLUDE.** Nothing enters the tree except by passing a layer's contract. An
  unlisted import needs a ruling before it arrives.
- **No condemned source in-tree, and no comparison binary, ever.** There is no
  `retired/` directory and no oracle binary kept for output comparison.
- **Weight lives at the bottom.** SQLite carries the work.
- **Contract before code.** A layer's contract lands before the code that satisfies it.
- **One obvious path per problem.** A second spelling of an existing operation is a
  defect *even when its output is correct*.

## Decision

### 1. Restart, blank repo, bottom-up (R1)

The line restarts rather than being repaired in place or stopped. Build order is
bottom-up, with the enforcement machinery landing first. **The layers are numbered, and
the rest of this ADR refers to them by number:**

| Layer | Name |
|---|---|
| 0 | Contract scaffolding — the enforcement machinery |
| 1 | Substrate |
| 2 | Lockdown |
| 3 | Queries |
| 4 | Mutations |
| 5 | Repair |
| 6 | Surfaces |

[`docs/architecture.md`](../architecture.md) carries the same numbering in its layer
landing map, which states which crates each layer brings.

Repair-in-place is rejected because it preserves the skeleton the wrong build order
produced, 83% of the existing test mass is implementation-shaped drag, and both pillars
of the new work (a real filesystem watcher, an HTTP surface) are greenfield either way.

The restart is a **triad**: fresh code repository, fresh vault workspace, fresh Mimir
project. The legacy repository, workspace, and board are **quarried** — read freely as
evidence, and mined for material that enters only by the permitted import route of
section 2 — never inherited wholesale.

### 2. A fresh repository, not a fresh root (R2)

The new line lives in its own repository. Condemned source sitting in-tree was the
contamination vector on the previous line — shapes leaked out of a `retired/` directory
across two phases *and a retrospective*, three recorded leak events. A separate repository
makes "no comparison binary, ever" true by construction.

- Quarry enters by **copy-plus-review only**.
- Git history stays in the legacy repository.
- Anything the new tree needs from that history is carried as a document, not as commits.

### 3. Name, releases, and the rename hazard (R3)

The product is **norn**. The previous repository is renamed **norn-legacy** and becomes a
maintenance line (critical fixes only; interim releases continue from it); a fresh `norn`
repository then takes the name.

**Rename status: decided, not landed.** The rename is parked at Mimir **NORN-5** until
swap-over. Until it executes, the previous repository stays live at `dbtlr/norn` and this
one has `dbtlr/norn-next` as its upstream. The scaffold deliberately precedes the rename:
the tree is built and reviewed before the name moves.

Verified hazard: installed binaries hardcode their self-update endpoint at
`https://github.com/dbtlr/norn/releases`, so **after** the rename that URL resolves to this
repository. Therefore:

> **This repository publishes zero GitHub releases of any kind until graduation. The
> first release IS the graduation event.**

The hazard arms at the rename, not before, and nothing in this repository enforces the rule
today — no release workflow exists to enforce it against. It is a standing prohibition on
the humans and agents operating the repository, and the rename task carries the obligation
to verify it holds at swap-over.

Graduation is gated on **capability parity** — the jobs the shipped binary serves, served
— judged at the jobs level against the re-derived surface. It is never gated on output
comparison. [Section 14](#14-paper-trail-and-graduation-r17) states the operational gate.

### 4. Versioning (R4)

The 0.x line continues. The graduation release is the next minor above wherever
`norn-legacy` sits at cut time. This repository carries a development version that never
ships. **v1.0 is reserved** as a separate, deliberate contract-stability declaration,
decoupled from graduation.

### 5. Workspace and board identity (R5)

- Mimir: the new project key is **NORN**. The `NRN` board is untouched as history.
- Vault: the `norn` workspace moves to `norn-legacy`, executed with the legacy binary so
  backlinks rewrite as part of the move, including the brief file's stem — the fresh
  workspace owns the `norn` stem. Existing links to `norn` are rewritten to the renamed
  target.
- A fresh `norn` workspace is scaffolded from this repository.

### 6. Legacy work-state disposition (R6)

1. Non-terminal `NRN` tasks are **bulk-abandoned** with one recorded reason:
   *"2026-07-27 restart decision — line superseded by the NORN board; record retained as
   demand evidence."* Nothing is deleted; done and abandoned history is untouched.
2. **Migration is mining, not copying.** The frozen board is a catalog of demand
   evidence. Each layer's design pass sweeps it for tasks bearing on that layer; a task
   is recreated on `NORN` only when a layer's contract makes it real, citing the old ID.
3. Seeds **NRN-s21 through NRN-s28** get one explicit triage pass at disposition time.
4. Artifacts **NRN-a1 through NRN-a65** stay in place and are referenced by ID.
5. `NRN` stays alive for exactly one purpose: rare legacy hotfixes. The renamed
   `norn-legacy` repository remains bound to that board, which is what keeps the hotfix
   path usable.

### 7. The test contract — three strata, dormant then activated (R7)

**Stratum 1 — the coverage corpus.** 158 recorded cases enter the repository as **skipped
integration tests: inert drafts with zero authority**. A case is an argv line, a fixture,
the recorded **output**, and a hand-verified exit class — verified **under the re-earned
tri-state exit contract**, not under whatever exit contract the legacy line shipped. The
exit contract is re-derived here; the recording is only evidence about what a command
produced.

Input and output pairs are recorded verbatim from the rewrite binary pinned at
`76af2c3b`, whose output surface embodies the 56 behavior rulings of stratum 2. **That
link is why the two strata attach:** a corpus case shows what the command emitted, and the
ledger ruling attached to that verb says why it emitted that. Judging one without the
other is judging half the evidence.

Recording is a one-time extraction executed in the legacy checkout under a hermetic spawn
environment. **Extraction boundary** (NORN-a5): the rewrite binary never enters this tree,
its CI, or any later step — only recorded data crosses.

**Activation is the approval act**, performed per command through a front gate:

0. **Is this output independently good?** A recorded case is evidence, not a standard; a
   recorded output that is bad stays bad, and the gate stops here.
1. Does the shape change? If no — enable as-is.
2. If yes — either a mechanical input/output migration (rewrite the seeds, enable) or a
   semantic divergence (a recorded ruling lands first).

Cross-surface inconsistencies migrate as **class rulings decided once and swept
mechanically** — one grammar, not per-command drift.

**Erratum, corrected here.** R7's prose says "the 8 unported stubs" and then enumerates
**nine**: `init`, `completions`, `cache`, `config`, `self-update`, `serve`, `service`,
`audit`, `manpage`. The enumerated set of nine governs; the count "8" is a recorded erratum
(NORN-a5, ground-zero imports table).

**What the nine bind.** They are **unseeded** — the corpus holds no cases for them at all,
so they can be neither activated nor retired *as cases*. The resolution:

- The zero-skipped judgment of [section 14](#14-paper-trail-and-graduation-r17) is
  evaluated **over corpus cases**, and only over corpus cases.
- The nine-member set bounds what that judgment **proves**: zero skipped cases does not
  imply these nine commands are covered, because nothing was ever recorded for them.
- **Their disposition is the verb charter's job** (section 13.1) — each of the nine either
  earns a place under the re-derived surface with tests written fresh, or does not exist.

Recording the count as nine matters precisely because it fixes the size of that gap.

**Stratum 2 — the behavior ledger.** 56 behavior rulings carry **per-approval**. Each
attaches to its verb and is re-judged when that verb activates, entering as contract only
on an affirmative "still right under the re-derived surface" judgment. A verb the charter
deletes takes its rulings with it, retired by recorded ruling (NORN-a5).

**Stratum 3 — the regression stratum.** Measured legacy defects — mined from the frozen
board and the reckoning appendices (NRN-a59..a63) — become named integration tests
asserting this system does not exhibit them. The named set at ratification: scoped
validation must cost less than whole-vault validation; a path glob must never void
`LIMIT`; dead flags are structurally impossible; warm requests assert zero derivation
counters; size-independence guards cover reads and mutations.

**Posture across all three strata: properties and counters over byte comparison.** Bytes
pin rendering, and only at Layer 6. Properties pin the mission.

### 8. Fixtures and CI (R8)

**Generator knobs are the realism contract:** document count, seeded body-length
distribution (long-tailed — the previous generator's ~20× body-size gap is a named
defect), link density, **ambiguity-class size k**, heading density, directory-tree shape,
and non-Markdown clutter (walk costs scale with directories).

Calibration is a measured artifact: a statistics probe against a real vault yields
checked-in generic parameters. Recalibration is a deliberate act. Generator determinism —
the same `(profile, seed)` producing the same tree, byte for byte — carries forward, as
does the zoo corpus, a checked-in fixture asset imported under NORN-a5's ground-zero
inventory alongside those calibration parameters.

**CI is two-tier: counters gate, clocks trend.**

| Lane | Trigger | Asserts |
|---|---|---|
| Per-PR gate | Pull request, push to the default branch | Derivation counters (zero on warm paths), `EXPLAIN` plans against the builder's actually-emitted SQL, size-independence pairs (counts, never clocks), the memory invariant — at a ~2k-document realistic profile |
| Soak | Schedule + manual dispatch | Wall-clock bars, peak-memory trend lines ("only goes down"), ≥5k-document profiles |

Wall-clock bars also run as **local pre-merge checks**, which land with the Layer 0 CI task
(NORN-13) alongside the soak lane they share bars with. **No wall-clock pass/fail ever runs
in per-PR CI.**

### 9. Topology — one supervised host, N registered vaults (R9)

One host process serves all registered vaults. This deletes an entire complexity class:
the per-vault summon ladder, socket naming, spawn claims, reap races, and orphan garbage
collection.

Two historical cautions argued for a process per vault. Both are dissolved by decisions
already made here, and these are the standing answers when the objection returns:

1. **"A wedged mutation takes down every vault."** It does not. Workers are killable
   threads inside the host; a wedged mutation is a killed worker, not a killed process.
2. **"Restarting the host is expensive for every vault at once."** It is not. Databases are
   durable and already healed, so a host restart is cheap re-vouching — not re-derivation.

The third argument is cost: the custodian is thin, so the marginal cost of one more
registered vault is negligible, which is what makes serving N of them from one process the
simpler shape rather than merely the cheaper one.

- **Lazy attach** bounds file-descriptor and watch usage; the fd budget is a measured
  Layer 1 acceptance item.
- **Registered vaults remain the central concept.** The registry is the host's serving
  set. Registration gates durability — durable database, watcher, warm trust.
  Unregistered roots get disposable derivation.
- Per-vault exclusivity carries as flock-then-bind, applied per vault entry.

### 10. Always routed; cold is a state, not a path (R10)

**Every product request routes to the host. No in-process serving mode exists.** The host
binary has exactly two lifecycles: a supervised service (the product path) or an
auto-launched TTL instance (degenerate mode — supervisor off, TTL on). One attach seam
serves both.

Recorded rationale, for anyone who wants to re-open it:

1. A direct path is a parallel behavioral surface — progress, errors, streaming,
   concurrency — and a direct/routed fork breeds bug layers.
2. Callers work in bursts; TTL warmth makes every command after the first fast on
   machines without the service installed.

The hazards that made the previous summon ladder dangerous are already gone: one
well-known socket (no per-vault × config × build naming), durable databases (spawn races
and TTL exits carry no expensive stakes), in-band version negotiation, and a self-watchdog
posture for unsupervised hangs.

**"Cold" is a state of a vault entry inside the host, not a code path.** First attach runs
`attach = heal-then-ready`: the deliberately unoptimized full heal (add missing, update
changed, prune deleted), billed once to the first request under a framed `Warming`
progress display. Warm thereafter, watcher-vouched. Mutations are permitted on first
attach; they are simply slow, honestly.

This is a deliberate partial reversal of the *grounds* on which the previous line argued
always-routed (legacy ADR 0017) — trust rules are now explicit per state under a durable
database — while keeping its one-path conclusion.

### 11. Embeddings — determinism preserved (R11)

The product's "deterministic, no LLM in the loop" commitment cashes out as
**reproducibility plus no-model-decides**. A pinned, versioned, local embedding model
violates neither, so semantic search is admissible under three doctrine-level guardrails:

1. **Semantic search is a query surface, never a correctness input.** No finding, plan,
   resolution, or refusal ever depends on a vector.
2. **Vectors are versioned, derived, rebuildable state** — pure functions of
   (model id, version, content). A model upgrade is a migration of derived state.
   Inference is local only.
3. **Honest consistency split.** Full-text search is maintained transactionally in the
   increment; embeddings are eventually consistent via async workers, and the surface says
   so.

CI uses a deterministic stub model; the real pinned model runs in the soak lane. Four
schema pillars enter at Layer 1: the FTS5 table, the vector table, the **findings table**,
and **real migration machinery**.

Implementation choices — model selection, storage shape, and the mechanics of fetching and
pinning weights — defer to the Layer 1 design pass. This section fixes the guardrails, not
the implementation.

### 12. Lockdown — five measurable gates (R12)

Lockdown is the recorded event at which the substrate contract freezes. It requires all
five:

1. **The raw-SQL acceptance contract green at scale** — count-by-field, suffix/stem
   resolve, links-to, predicate+sort+page, findings-for-path, FTS match, and
   vector-nearest-via-stub — each with timing bars, `EXPLAIN` on emitted SQL, counters,
   and memory.
2. **Watcher fidelity** — a seeded churn suite (bursts, atomic replaces, branch flips,
   mid-mutation edits). The bar is convergence-to-equivalence with a from-scratch build,
   with the settle bound expressed in *work* (counters proportional to the changed set).
   The auditor half is proven by killing the watcher.
3. **Induced-failure suite** — `kill -9` mid-increment, disk full, permission loss,
   corruption injection, stale schema. Every rung of the heal ladder is reached by a test.
4. **Soak** — nightly ~1h mixed load with zero counter violations, a flat memory slope,
   and no fd growth. Lockdown requires 5 consecutive green runs.
5. **The recorded event itself.** After lockdown, substrate-contract changes require
   explicit ruling entries.

### 13. Verbs, findings, mutations, repair, surfaces

#### 13.1 The verb charter (R13)

Layer 3 opens with a **jobs-first design pass** — callers × jobs → verb set, ADR-shaped,
landing before any Layer 3 code. Pre-rulings that bind the charter:

- Recorded corpus verbs are **presumptive-carry**; the activation gate (section 7)
  handles shape. The nine unseeded commands of section 7 have no corpus standing at all
  and are judged here from scratch.
- Verbs belonging to the condemned topology either die or re-derive against the one-host
  model: `cache`, `serve`, `service`.
- **One-obvious-path binds the charter.** Full-text search, semantic search, and `find`
  must resolve to sharply bounded jobs, not overlapping spellings.
- New-world operability earns a surface: trust state, heal history, watcher liveness.
- Distribution verbs carry.
- **Help doctrine graduates** — the short/long stance, the custom renderer, and per-verb
  earned prose, re-earned verb by verb.
- **Live examples sunset.** Help is a pure function of the binary and its arguments, never
  a vault load.

#### 13.2 Findings bound and the resolution grammar (R14)

`Finding.candidates` is the one measured superlinear term (O(k²), 153× payload growth). It
becomes a **bounded head of 5** in deterministic resolution-ladder order, plus
`candidates_total` — bounded in the findings table and on the wire alike. Full enumeration
is a query-layer job (indexed, reachable via a machine-readable hint carried in the
finding). Repair reads the live ambiguity class, never the finding's snapshot. The
ambiguity-`k` fixture knob proves the bound.

**Resolution grammar (one grammar, every target surface — CLI, MCP, wikilinks, filters):**
targets resolve by right-to-left, segment-aligned path suffix. `glossary` matches any
`**/glossary.md`; `norn/glossary` matches only `**/norn/glossary.md`. Stem resolution is
the one-segment case of the same ladder. Candidates emit as minimal disambiguating
suffixes. Suffix resolution joins the raw-SQL acceptance contract with its own `EXPLAIN`
bar and index support.

#### 13.3 Mutations (R15)

Verb order adds exactly one mechanism per landing:

`set` → `edit` → `new` → `delete` → `move` → `rewrite-wikilink` → `apply`

- **Everything is a plan.** Every mutation verb compiles to one re-derived typed plan
  vocabulary; one applier executes. `apply` is the verb that accepts externally supplied
  plans.
- **Write-through is the core contract.** The worker composed the post-state, so the
  increment writes it: database updates scoped to the blast radius, with re-derivation of
  composed state counter-guarded to zero.
- **Drift policy is refuse-and-refresh.** Per-file transaction discipline carries
  (fingerprint → shadow → verify → swap). Detected drift refuses and returns a fresh
  forecast whose content hash rides the confirmation as an implicit compare-and-swap.
  Auto-rebase-on-drift is deliberately rejected: it is cleverness on the failure path, and
  a changed world deserves a re-plan.

#### 13.4 Repair and surfaces (R16)

- **Repair is a planner over the findings table**, feeding the same single applier. Scoped
  evaluation carries the scoped-equivalence claim together with its coverage assertion.
  Plans cite the finding generation they were planned against.
- **Params and reports are defined exactly once** — a single wire-types vocabulary. CLI
  flags and MCP tool schemas are *derived renderings*. A surface-specific parameter is a
  defect; surface-specific presentation is fine. This inverts legacy ADR 0009, which is
  cited as the decision being replaced.
- **Auth.** HTTP MCP binds loopback with a static bearer token generated at service
  install. TLS, multi-user, and remote access are named out of scope until a real remote
  consumer exists. Auth stays middleware-shaped so that remains true cheaply.
- **stdio MCP is a thin secondary** over the same vocabulary.
- **Registry.** One user-level config-directory file mapping name → root plus per-vault
  settings, mutated only through verbs. Strict two-config separation: the registry says
  *which vaults and how served*; a vault's own config carries *its doctrine*, pinned at
  attach, with re-attach on change.

### 14. Paper trail and graduation (R17)

**ADRs are re-authored at the layer where they bind** — fresh numbering, present-tense
contracts citing legacy ADRs and measured evidence by reference. Nothing bulk-copies. This
ADR is 0001 of the line.

[`docs/architecture.md`](../architecture.md) enters at Layer 0 as the **invariant spine**:

- memory as a measured invariant,
- default-EXCLUDE,
- the heal ladder,
- raw-SQL acceptance,
- one-obvious-path.

The previous line's `cache.md` has no successor; the concept it documented does not exist
here.

**Migration story for existing installs:** vault files are untouched; first attach is the
cold build; a first-run janitor cleans orphaned legacy cache directories; config-schema
evolution goes through the migrate verb.

**Graduation gate, operationalized.** All six:

1. **Zero skipped corpus cases** — every case in the corpus either activated or retired by
   recorded ruling. This is judged over corpus cases only; the nine unseeded commands of
   section 7 are outside it, and their disposition is the verb charter's, not this gate's.
2. Lockdown bars green, including soak.
3. Verb charter fully landed.
4. A real-vault burn-in period.
5. Self-update version handling verified.
6. Docs covering the shipped surface.

The first release is graduation.

**Schema version resets to 1 and holds through the restart build.** Development-time DDL
changes rebuild from zero via a DDL fingerprint stored in the meta table, consuming no
version numbers. Version 1 freezes as the first migratable baseline at graduation.

### 15. Execution sequencing (R18)

The `NORN` board seeds **structure only**: a ground-moves phase plus the Layer 0 tasks.
Later layers get tasks when their design passes open.

Ground-move order:

1. Workspace move and board disposition — so there is no window in which two things answer
   to `norn`.
2. Repository rename.
3. Fresh repository scaffold.
4. Layer 0.

GitHub-touching operations run only on explicit go. Two design passes precede any
implementation dispatch: the **crate-boundary map** (NORN-a4) and the **carry-over
inventory** (NORN-a5). Both have landed; `docs/architecture.md` carries the crate map
forward as repository-authoritative.

## Consequences

- **The tree grows only by earned admission.** Every file is either fresh authorship or an
  import listed in NORN-a5, and adding to that list is itself a decision. Reviews check
  admission, not just correctness.
- **Enforcement precedes product.** Layer 0 builds fixtures, harness, and gates before any
  vault code exists. The first visible progress is slow and produces no user-facing
  capability; this is intended.
- **No releases until graduation.** Once the rename lands, the self-update endpoint of
  installed binaries resolves here, so any release would ship to them. There is no
  partial-release escape hatch, and no in-repo mechanism enforces the rule — it binds the
  operator.
- **Recorded legacy behavior is inert by default.** Three dormant artifacts sit in the tree
  or the workspace with zero authority: the corpus and the behavior ledger of section 7,
  and — per NORN-a5 — a source-cited help-prose catalog keyed by verb, rendered nowhere and
  consumed only at a verb's activation. Every activation is an explicit judgment, which
  makes the surface slower to assemble and much harder to inherit by accident.
- **One host process is a single point of failure by construction.** The trade is accepted:
  it deletes the summon/reap/orphan complexity class, and durable databases plus cheap
  re-vouching make host restart uneventful.
- **One-obvious-path is enforceable and will reject work.** A correct implementation of an
  operation that already has a spelling is a defect and gets rejected on those grounds
  alone.
- **Some previously-shipped behavior will not return.** Capability parity is judged at the
  jobs level; verbs belonging to the condemned topology can die outright, and the charter
  is free to reshape the rest.

## References

All identifiers below are Mimir IDs and resolve on the `NORN` or `NRN` board.

- **NORN-a1** — restart decision of record (frozen source; the eighteen rulings).
- **NORN-a4** — crate boundary map, carried into
  [`docs/architecture.md`](../architecture.md) as the repository-authoritative form.
- **NORN-a5** — carry-over inventory: what enters the tree as bytes, and by which gate.
  Source for the R7 stub-count erratum (section 7), the corpus extraction boundary
  (section 7), the retirement of a deleted verb's ledger rulings (section 7), the zoo
  corpus and calibration import (section 8), and the dormant help-prose catalog
  (Consequences).
- **NORN-5** — the parked repository rename (section 3).
- **NRN-a59 … NRN-a63** — the reckoning appendices: the audit whose measurements are cited
  in Context and whose recorded misbehaviors seed stratum 3 (section 7).
- **NRN-a1 … NRN-a65**, **NRN-s21 … NRN-s28** — legacy artifacts and seeds, retained in
  place (section 6).
- Legacy **ADR 0009** and **ADR 0017** — the prior-line decisions replaced in sections 13.4
  and 10.
