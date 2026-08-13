# Architecture

`norn` is a Markdown-vault engine: it derives queryable state from a directory of Markdown
files, keeps that state current as the files change, and applies planned mutations back to
them.

This document is the invariant spine and the crate map, and it is the **authoritative form
of both** — the contract reviews enforce against.

The design passes that derived this content are recorded as Mimir artifacts: **NORN-a1**
(the decisions taken before this repository existed), **NORN-a4** (the crate-boundary
pass), and **NORN-a6** (the Layer 1 substrate pass). Those are evidence about how the shape
was reached, consultable and citable; they are not authority. Where they and this document
differ, this document governs. Decisions taken from here are recorded as ADRs only when they
pass the admission bar in [`AGENTS.md`](../AGENTS.md); implementation cadence creates no
documentation obligation. The qualifying durable rationale is indexed in
[`decisions/README.md`](decisions/README.md).

---

## Part I — The invariant spine

These invariants bind the whole system. Each section below names its own gates where it has
them, and marks the judgments that are review acts instead. When a gate arrives is not this
document's concern; the comments in `.github/workflows/` are the in-repo operational marker.

Where a gate exists, its lane depends on what it measures: **counters and structure gate per
PR; clocks and trends live in the soak lane and never fail a pull request.** [ADR
0004](decisions/0004-two-tier-measurement-and-authored-baselines.md) records why evidence
kind, rather than observed runtime, owns that division. A review-held judgment has no lane
at all, which is why each section says so where it applies — a review-held invariant rots
quietly.

### 1. The memory invariant

**Peak memory is a function of the working set, not of vault size.** A request that touches
ten documents costs the same whether the vault holds one thousand or one hundred thousand.
Nothing streams a whole vault through RAM — not a walk, not a query, not a mutation plan.

The invariant is *measured*, not asserted:

- Per-PR CI asserts peak memory at a realistic ~2k-document profile, alongside
  size-independence pairs expressed as counts rather than clocks.
- The scheduled soak lane records the peak-memory reading each run and bars it
  against an authored baseline. Under [ADR
  0007](decisions/0007-authored-measurement-thresholds.md), the baseline moves
  only by a reviewed edit, and the downward direction of that movement is
  review-held rather than mechanized — nothing here fails a raised baseline,
  so this is a place this section's own rule applies: a review-held invariant
  rots quietly.
- The mechanized form is a flat-slope requirement, and it runs in the same
  lane: a host attaches the ≥5k profile's vault and works it under a nightly
  mixed load while sampling its own resident set, and the run compares the
  first quartile's mean against the last. That comparison is one the run makes
  against itself, so no recorded history decides it.

Superlinear payloads are defects, and bounded ones stay bounded on the wire and at rest
alike — see the [bounded candidate head of 5](#the-crates) in the `norn-wire` membership
rule.

### 2. The heal ladder

Derived state is always rebuildable, and the system reaches for the cheapest rung that
restores trust. Three rungs, cheapest first:

1. **Scoped increment** — the watcher reports filesystem facts; the increment touches only
   the changed set. This is the warm steady state.
2. **Full tree heal** — attach and recovery establish watcher coverage, heal, drain the
   filesystem facts accumulated during the heal, then publish `Ready`. The coverage boundary
   and drain are defined by [ADR 0017](decisions/0017-watcher-synchronization-gates-readiness.md).
   The heal adds missing, updates changed and prunes deleted. "Update changed" is decided by a
   content hash, never by a stat comparison — see [the trust model](#5-the-trust-model).
   Deliberately unoptimized, and billed once to the first request under a framed `Warming`
   progress display. Its resumable-friendly shape is carved for a later stat-prioritized,
   progressive-verification evolution; nothing measures it today.

   **The prologue comes before the heal, and the display says which of the two an entry is
   in.** Before a document is read, an entry takes sole maintainership of its own derived state,
   resolves and sweeps its shadow home, establishes change detection over the tree, and
   opens the derived state a read answers from. The first request is billed for all of it,
   and none of it counts a document — which is why `Warming` carries a typed phase beside
   its counters rather than counters alone. An entry that has not finished that prologue
   and an entry whose heal is not advancing are different waits, and zero healed against an
   unknown total is the whole truth of both.
3. **Rebuild from zero** — the derived database is discarded and rebuilt.

Vault files are never the thing being healed; they are the source of truth. The ladder
splits across the two effect seams: tree-walking rungs orchestrate through `norn-fs`,
database-side rungs (store schema fingerprint, rebuild-from-zero) live in `norn-store`. A rung
may be subdivided; a fourth escape hatch may not be added.

**Rung 3 has two triggers, with different lifetimes.**

- **A DDL change during development.** Store schema version is pinned at **1** and does not move
  through the pre-release build. A DDL edit is detected as a fingerprint mismatch in the
  meta table and resolved by rebuilding from zero, consuming no version numbers. This is
  the development-time path and it exists because version numbers are worth more than the
  churn they would absorb.
- **Corruption, or any state the lower rungs cannot resolve** — at any time, before or after
  release.

At the first release, version 1 freezes as the first migratable baseline. From then on the
**migrations pillar is the store schema evolution path**, and rebuild-from-zero narrows to the
second trigger alone.

**The store types damage; the host routes it.** An open resolves the damage it can see for
itself, and it can see only the store schema — so a page that reads corrupt under a warm
increment, a full-text index that stopped agreeing with the column it indexes, or a value
outside a closed vocabulary is damage a *later* operation meets. Every such refusal is typed
as damaged state at the operation that met it, and the driver codes that qualify are the
store's judgment to make because SQLite is the store's to own. What the host does with the
verdict is the rung: trust is withdrawn under a reason of its own, the store is consumed and
its file discarded, a database is created in its place, and the vault is derived into it by
the same heal an attach runs. **A damaged verdict never re-enters the lower rungs** — a
retry, or coverage re-installed over the same database, meets the same page again — so the
requirement damage sets dominates the one a broken environment sets wherever both stand.
Watcher coverage and the maintainer lock stand through the rung: neither is what was damaged.

**An attach runs the rung inline, and publishes no verdict for it.** An entry holding no
coverage owes an attach before it owes a rung, so a verdict published from an attach would
be answered by a second attach against the same file and the same page. The attach that
meets damage in its own heal therefore resolves it where it stands, and a rung-3 run reached
that way is not observable in the trust stream — the same silence a rebuild-from-zero at open
time already carries. What a client sees is Warming, then Ready. Every other route publishes
the verdict, because every other route is an entry that holds the store the rung will replace.

**Silent damage is asked about on a schedule.** The verification that compares the database
against itself is the only thing that meets damage no read fails on, so it runs as bounded
lifecycle maintenance beside the shadow sweep — off the request path, never per request.

**Every rung is to be reached by a test, across two suites**, and one of the two is built.
Each suite below says where it stands.

Rung 1 is the **churn suite**'s — bursts, atomic replaces, branch flips, mid-mutation
edits — whose bar is convergence-to-equivalence with a from-scratch build and a settle
bound proportional to the changed set. It is the warm path, so it is reached by ordinary
operation rather than by injected failure. **That suite is not built**, and its absence
binds here the way an absent mechanism does: rung 1 is reached by no test until
`norn-testkit`'s churn driver and the host suite that runs it exist.

Rungs 2 and 3 are the **induced-failure suite**'s, whose lane runs per PR. The table below
is its contract — each row an injection and the outcome required of it — and two of its
rows are unreached: nothing stands a full disk or a revoked permission in front of a rung-2
heal over vault paths. Both conditions are arranged elsewhere for other bars — `norn-fs`
injects the shadow write that cannot take its content, which is the full-disk arm of its
own write protocol, and its suites revoke permissions to reach their own refusals — so what
these two rows wait on is the heal-side injection they are stated over.

| Injected failure | Required outcome |
|---|---|
| Process killed mid-increment | **Handled at rung 2.** **The changeset is the unit of atomicity**, and an increment is one or more of them: a changeset lands whole or not at all, so a process that dies inside one leaves the store holding no part of it. A heal-scale increment is chunked into separately atomic changesets, so a tear between two of them leaves every chunk that committed and no part of the one in flight — each generation whole, the vault's coverage short. Either way the work the tear lost returns through the attach heal's ordinary content-hash comparison. One flush is a changeset plus the findings recorded after it, each in its own transaction, so a tear between the two leaves the increment landed — a tombstone where a quarantined path had a row, the derived row where a document read without its frontmatter — with no finding beside it until the next heal records one. Each is reached without an edit to the file: no row stands at a quarantined path, so the walk reads it; a degraded row states its own defect, so the walk re-derives it when nothing stands beside it. A tear inside one changeset is injected today; a tear at a chunk boundary is the suite's to reach. |
| Disk full | **Refused at rung 2.** The increment cannot complete, the entry stays untrusted, and the request refuses saying so. |
| Permission loss on vault paths | **Refused at rung 2.** An unreadable path is an error, never evidence of deletion: the heal refuses rather than prunes. |
| A document norn cannot decode | **Quarantined at rung 2.** The document yields no facts, a finding names it and the cause class — withheld while a readable document stands at its rendered spelling — the heal keeps going, and the entry reaches `Ready` serving every other document. |
| A document whose frontmatter block is read by nothing | **Degraded at rung 2.** The document keeps the row the act could derive — identity and body facts, no frontmatter projection — and a document-scoped finding naming the cause stands beside that row until an ordinary re-derivation finds the block readable. |
| Corruption injection | **Handled at rung 3.** The database is discarded and rebuilt — by the open where the corruption is in the store schema, and by the host's rung-3 leg where a warm read, a warm write or the scheduled verification is what met it. |
| Stale store schema (DDL fingerprint mismatch) | **Handled at rung 3.** Pre-release — which is what the suite asserts today — a fingerprint mismatch means the DDL was edited, and the database is discarded and rebuilt. Once version 1 freezes as the migratable baseline, store schema *evolution* becomes the migrations pillar's job, and rung 3 is reached by a store schema that is damaged rather than merely out of date. |

**Refusal is resolution.** Rung 3's second trigger reads "any state the lower rungs cannot
resolve", and a transient environmental failure does not qualify. When the disk is full or a
permission was revoked, refusing and leaving the entry untrusted *is* the lower rung
resolving the situation correctly: the environment is broken, the stored state is not, and
discarding a sound database would destroy work to fix nothing. **Rung 3 is for damaged
state, never for a hostile environment.**

**Enumeration and open are two observations of a moving vault.** A heal walks the tree and
then opens what the walk named, and a foreign edit can remove or replace an enumerated path
in between. That window is ordinary churn rather than a broken environment, so it converges
instead of refusing: the answer is the one a walk begun now holds, and it is one answer for
a name that was deleted and for a name that a directory, a link or a pipe took — no document
is there, so the row standing at it is pruned and nothing is derived for it. Only the
machine's own failures — a denied directory, an exhausted descriptor table, a failing device
— still refuse at that window, which is what keeps the permission-loss row above true. The
walk's own paging of a directory is a narrower window and does not converge yet: an entry
that is unlinked or changes kind between the listing that named it and the stat that reads
it refuses there.

**Quarantine is per document; refusal is per vault.** See
[ADR 0019](decisions/0019-quarantine-is-for-nothing-derivable-causes.md), which supersedes
ADR 0018. A document norn can derive nothing from — path bytes that are not UTF-8, a path
spelling the document-path grammar refuses, or a body that is not UTF-8 — is a fact about one
document rather than about the vault, so it never withdraws the vault. The rung-2 heal skips
its facts, records a finding naming the path and the cause class — withheld while a rendering
collision stands, as below — and keeps going; the entry reaches `Ready` and serves every
other document.
Every rung-2 path answers the three cause classes the same way: the full tree heal, a scoped
subtree heal, and the warm scoped increment. A dirty **directory** whose own spelling the
grammar refuses splits by what it still addresses, because naming no document and holding no
document are different things. A spelling refused only for the stem its leaf reduces to —
`..md` — names no document and is where ordinary documents such as `..md/note.md` are
stored, so it addresses every row beneath it as a segment-aligned prefix range: the warm path
merges its walk against that range and derives, quarantines and **prunes** there, converging
a deletion under it without a vault-wide heal. A spelling refused for anything else — a
backslash, a control byte, bytes that are not UTF-8 — spoils every path beneath it too, so no
document under it is storable and there is no row to prune; the warm path reads what is under
it and quarantines what it finds.

**A document whose frontmatter block is read by nothing degrades rather than quarantining.**
A block that never closes, a block that is not well-formed, and a block past the authored
`FRONTMATTER_MAX_BYTES` bound are one situation under three causes: the block is unread, so
the document's fields are unknown rather than empty. The act derives what it could — identity,
body, headings, links, body tags — the frontmatter projection is absent, and a
**document-scoped finding** naming the cause stands at the document's own path, beside its
row. Where the body starts differs by cause: a closed block bounds its own bytes, so an
oversized or unreadable one is skipped and contributes nothing, while an unclosed one bounds
nothing and the document is body from its first byte — the links, headings and tags written in
the lines that opened like a block are read as the document's own. The bound still refuses
the block whole and nothing is truncated; what the refusal produces is that row and that
finding rather than a removal. The finding closes on the ordinary derivation that finds the
block readable again, so a document edited across the bound or across a typo moves its
finding and never its row.

The store holds only representable truth, so a document that stops decoding **loses its store
row**, and the finding is where its absence is stated. The row's death is recorded with the
quarantine provenance, which says the derived row died and the file did not; a prune or a removal
would say the path left the vault. A quarantined path is filed under the path the vault spells it,
and under a rendering of that spelling where the grammar admits no such path. **A rendering names
a place, never an identity**: no document is *derived* under one, though the vault may genuinely
hold a document whose own name is that place. That collision is one key holding one row, and the
**document wins**: while it stands there, the quarantined document's place-scoped finding is
withheld rather than filed over a readable document, so for as long as the collision lasts nothing
records that the quarantined document cannot be read. The trade is deliberate — a finding at that
place would call a document that derived unreadable — and **the heal that removes the colliding
document is the one that clears it**: the deaths that vacate rendered places send the heal back,
after its last increment, to read the roots those places sit under, and every refused spelling
those readings meet is quarantined then. Waiting for a demand or for unrelated work to reach the
quarantined path is what that revisit exists to prevent.

**Findings carry two scopes, and one thing separates them: whether a document row at the subject
withholds the finding.** A **place-scoped** finding — every quarantine — says nothing is derived
at its subject, so a readable document standing there withholds it, as the collision above does. A
**document-scoped** finding — every unread block — is about the document derived at its subject,
so the row standing there is what it describes and nothing withholds it. A kind says which it is,
in the same registry that spells it, so a producer recording one and a client reading one read one
answer. **A finding a producer records replaces the findings it re-derives and no others, and the
unit that carries that scope is the finding rather than the job.** What a quarantine replaces at
the place it is filed at is decided by what the act that derived it read. An act that read a path
and opened no bytes — the revisit, the sweep of a root the grammar poisons, a dirty path the
grammar refuses — concludes what a spelling alone decides, so a quarantine about the document
standing at that place is left where it is. An act that opened a document's bytes and refused them
concludes what those bytes say, so the findings about the refused spellings rendering onto the
same place are left where they are. Each side is the causes it can conclude, read off the cause
the finding states, so neither side can take the other's work without re-deriving it. **One
discard still takes a place whole, and what licenses it is one reason per scope**: the increment
runs it once per changed path, because a change that writes a row there ends every place-scoped
finding at that place — none of which anything could read for as long as that row stands — and
takes the document-scoped ones the act that wrote the row concluded by reading the document.
Recording follows the increment inside one flush, so concluding and refiling is one act: a block
still unread is stated again where it stood, and a block that reads is stated nowhere. **A finding
whose subject the vault no longer holds is reached by neither side**: nothing reads a path that
left, so no walk re-derives what stands at its place and no discard takes it, and it stands until
a schema re-pin empties the table the vault-wide walk then fills again. That is the standing gap
in this convergence, and the collision above is where it is most visible — the document whose
bytes were refused can leave while a refused spelling still renders onto its place. **A
document-scoped finding is converged as a pair with the row it stands beside**, because a
hash-authoritative walk reaches it no other way: a place-scoped finding sits where no row does and
so is read on every walk, while a document-scoped one sits where a row does and would be reached
only when that document's bytes moved. A vault-schema re-pin discards it and a process killed
between a flush's increment and its recording never writes it, and either would leave a row
asserting an absent frontmatter with nothing saying the fields were never read. So the row states
its own defect — an absent frontmatter projection beside a nonzero count of frontmatter-scoped
diagnostics is a block nothing read — and the walk re-derives such a document when no
document-scoped finding stands at it. That is one indexed findings lookup per defective document
per heal, and a converged vault re-derives nothing. **The revisit
is opportunistic, and it is owed once per heal rather than once per removal**: its increments are
already committed, so a directory it cannot open ends that root's reading rather than refusing the
heal, and a place left unread that way keeps its finding withheld until a later heal reads it
again — the same honesty the paging window above states, for the same reason. Recovery needs no
second mechanism: a document that reads again is an ordinary derivation, and the increment's own
findings discard takes the finding with it.

Refusal stays for failures of the environment rather than of one document — a schema that
will not read, a store that will not open, a walk that cannot list a directory, a path whose
permissions were revoked. Widening quarantine past the undecodable-document class would turn
a broken environment into silent data loss, which is the hazard refusal exists to prevent.

**Cold is a state of a vault entry, not a code path.** There is exactly one attach seam and
both host lifecycles use it, so "cold" names rung 2 running on first touch — not a separate
code path with its own behavior.

### 3. Raw-SQL acceptance

**The store schema is the domain model at rest.** Reads compile wire params to SQL and let SQLite
answer. There is no repository tier and no domain-object hydration between the query and
the rows.

The acceptance contract is a fixed set of query shapes, each carrying timing bars, an
`EXPLAIN` assertion **against the builder's actually-emitted SQL**, derivation counters, and
memory:

- count-by-field
- suffix / stem resolve
- links-to
- predicate + sort + page
- findings-for-path
- full-text match
- vector-nearest (via the deterministic stub model)

Warm requests assert **zero** derivation counters. `EXPLAIN` gates run against emitted SQL
specifically, because a gate against hand-written SQL tests a string nobody executes.

The contract is stated whole and filled shape by shape, as each builder lands. The seam an
`EXPLAIN` bar is taken through exists — `norn-store` hands out the plan SQLite reported for
a statement it emitted, because a plan cannot be taken by a crate that may not open a
connection — and four named statements carry an index bar through it: suffix candidates,
findings in a class, and the class- and subject-scoped findings discards, the subject
discard in both the whole form and the form narrowed to the kinds a producer re-derives.
None of those is a query shape's bar. **No builder compiles wire params to SQL yet**, so no
query shape carries its acceptance `EXPLAIN` bar today. The warm-zero counter bar does gate
per PR, and no query shape carries a timing or a memory bar.

### 4. One obvious path

**A second spelling of an existing operation is a defect even when its output is correct.**
This is the strongest of them, because it rejects working code.

It binds concretely: one plan vocabulary and one applier; one document parser; one render
seam; one resolution grammar across every target surface; one invocation path to the host;
one owner for machine-local state. Where a new capability looks like an existing one, the
resolution is sharply bounded jobs — not two overlapping surfaces that each mostly work.

No mechanical check carries this. Whether a change is a second spelling is a judgment made
in review, and it stays that way until a rule can express it.

### 5. The trust model

**The vault filesystem is a shared surface.** People arrive through editors and sync tools,
never through norn's own surface, so every change norn learns about late is a stretch of time
in which the derived database answers from a stale world. High-fidelity change detection is
therefore what buys the right to read SQLite and answer, with no filesystem check on the
request path. The heal ladder is why imperfect detection cannot corrupt derived state;
detection fidelity is why that state is worth reading at all.

Two mechanisms carry trust, and both are contract:

- **The proactive watcher** — immediate detection, over a fidelity ladder across
  filesystems: native notification where the platform provides it. Warm trust is contracted
  for local filesystems today; backend selection at registration is the carved extension
  point for coarser detection elsewhere. Its contract is directional — **over-report freely,
  under-report never.** A redundant re-derivation costs work; a missed change costs a wrong
  answer, so every tuning choice biases toward the safe direction, and a lost-notification or
  overflow signal marks the entry untrusted for a rung-2 re-heal rather than being absorbed.
  **Reporting less and reporting nothing are two states, and they resume differently.** A
  watcher still covering the vault that dropped or overflowed notifications re-heals on the
  host's own dispatch; coverage that *ended* — a failed backend, a vault root that left the
  watch — is terminal, and the entry stays untrusted until a client demands it back. The two
  carry different `norn-wire` reasons, because a state that recovers by itself and a state
  that waits are not one fact. **Damaged derived state is a third reason on the same
  principle**: the environment refusing and the store being damaged resume differently — one
  waits for a machine to be fixed, the other discards the derived state and builds it from
  the vault on the entry's own dispatch — so they are not one word either.

  Watcher registration alone does not establish trust. A subscription first reports control
  state as `Synchronizing`, then `Live` only after its backend proves coverage over every
  registered edge; a synchronization failure is `Terminal(error)`. Attach and recovery wait
  for that boundary, run the hash-authoritative heal, reconcile the filesystem batches that
  accumulated during it, and only then publish `Ready`. Synchronization markers are not
  filesystem facts and never widen a batch to a vault rescan. The barrier does not mutate the
  vault, and a backend that cannot provide it refuses rather than silently changing the
  registration to polling. [ADR
  0017](decisions/0017-watcher-synchronization-gates-readiness.md) records the protocol and its
  trade-offs.
- **Background just-in-time drift scans** — idle-time, iterative, progressive
  re-verification of derived state against the files. This mechanism is not built yet, which
  makes it no less a contract: an absent mechanism binds here for the same reason an absent
  crate does.

Their pairing is what will make fidelity **empirical instead of asserted** rather than
leaving it a review-held claim: once the scans run, every drift a scan finds that the
watcher missed is a counted defect, trended in the soak lane.

**Hash authority.** Only a content hash concludes "unchanged" — anywhere in the system. A
stat fingerprint may prioritize work or raise suspicion; it may never conclude. The asymmetry
is why: a false "unchanged" destroys work or backs a wrong answer, while a false "changed"
costs a re-derivation. Reading and hashing a file is **one atomic act against one file
descriptor**, so the bytes hashed are provably the bytes read. Progressive verification
changes *when* a hash happens, never *what* concludes.

Fidelity telemetry and hash authority sit in different lanes. Detection-to-convergence is
the [churn suite](#2-the-heal-ladder)'s bar — convergence-to-equivalence with a
from-scratch build, which a watcher that under-reports fails — and that bar binds when the
suite exists, not today. Fidelity telemetry —
scan-caught misses per run — is a **soak-lane trend** and never fails a pull request. Hash
authority itself is **review-held**: no lint or suite yet forbids a stat comparison from
reaching a conclusion, so the invariant holds by review until one binds it, which is exactly
the case this part's own rule warns about — a review-held invariant rots quietly.

---

## Part II — The crate map

**Governing law: a crate boundary is earned** — by an effect seam, an enforced invariant,
heavy-dependency isolation, or a standalone future. Anything else is a module. This is the
crate-level face of one-obvious-path.

Twelve product crates (including the composition root), two development crates, one shipped
binary. Every crate carries a one-line **membership rule**. *The rule, not the current
content list, is what reviews enforce against* — a crate's contents are evidence about the
rule, never a substitute for it.

This document describes the whole target shape. The workspace holds only the crates that
have been earned so far; a crate's absence from `Cargo.toml` is not an absence from this
contract.

### The crates

| Crate | Owns | Earned by |
|---|---|---|
| `norn-wire` | **The vocabulary** — request params, reports, typed plans, findings, trust states. Pure types: no I/O, no logic. A finding's `candidates` is a **bounded head of 5** in deterministic resolution-ladder order plus `candidates_total`; the bound is wire shape and holds at rest in the findings table too. **Every enumerable code lives in one registry here** — refusal codes and finding kinds alike — under one grammar: a flat `namespace/what-happened` string is what a client branches on, a nested typed reason is structure inside a code rather than a code, a note a layer files about its own reading of one document is not a code and does not cross this seam, and an advisory `detail` string is prose no client matches on. A finding kind also carries the scope its findings stand at, so whether a document row at a subject withholds a finding is one answer the producer and the client read from the same registry. | Params and reports are defined exactly once; CLI flags and MCP tool schemas are derived renderings of these types. |
| `norn-text` | **The syntax of a vault document, never its semantics** — frontmatter parse / lossless edit / serialize, headings, sections, both link families (wikilink and inline Markdown, one fact shape carrying family, protocol and title, from which the resolution mode derives — protocol first, family second), `#tag` syntax (body tokens with code-span exclusion, plus the frontmatter tags shape). Frontmatter string values are scanned for wikilinks only. Pure functions over strings; answers "what does this document say", never "what does it mean" or "is it right". **A frontmatter block is read only up to an authored byte bound** (`FRONTMATTER_MAX_BYTES`), because the YAML scanner behind the seam is quadratic in block length on nested flow collections; a block past it is refused unparsed, so what a block costs to read has the ceiling the bound sets rather than growing with the block's own length — a ceiling, not a flat cost: inside the bound a nested block still costs about two orders of magnitude more than an ordinary mapping of the same length. Inside the bound, reading an ordinary mapping is **linear in its key count**, and so is deriving every field's strings back out of it: both places a block's keys are all resolved — the field split, over each scanned key line, and the text derive, over each field — go through a by-key view of the parsed mapping rather than a scan of it, and the soak lane bars both shapes by taking one whole block against four blocks of a quarter its keys. The `#tag` family is committed to graduate past syntax in two steps, in that order: the facet `norn-store` enforces belongs to the vault schema's content model and binds with it, and the query surface that reads the facet — whose shape the verb charter decides — comes after. | The one-parser invariant — every consumer reads documents through one grammar. Carve-out future: the serde-based frontmatter path can be replaced by a purpose-built parser without surgery elsewhere. |
| `norn-fs` | **Everything that touches the vault filesystem, and nothing that doesn't** — walk, read, stat/fingerprint, the atomic-write protocol (fingerprint → shadow → verify → swap), the per-entry flock primitive, the watcher as a subscribable stream of typed filesystem facts (debounced, coalesced, atomic-replace aware), and **the one path-spelling normalization point** — case, dot-prefix, redundant separators — so every consumer compares normalized identity instead of deriving its own. **A vault's own mechanism files are part of "everything that touches the vault filesystem"**: the maintainer lock file and the shadow home — one of each per registration, keyed by data base, channel and vault name — rather than among documents. The lock file is in the norn data root; the home is under the data root too unless the vault is on another filesystem, where it falls back under the vault root carrying that same key, so two registrations over one root stage into two homes and neither sweeps the other's. Creating, reading, writing and sweeping them is `norn-fs`'s — no other crate reaches them. Watcher coverage for an entry is three edges: the vault root tree recursively; the root's own parent non-recursively, which is the only edge that can report the root name being removed or renamed — coverage ending rather than a change inside the vault; and, when the entry's configured schema source sits outside the vault, that source's parent, added only when neither of the first two already reaches it. Filesystem facts only; not a general event bus. | The second effect seam; heavy-dependency isolation for the platform watcher backend; churn semantics unit-testable in-crate against a temp tree. Which backend wins is invisible outside the crate: no other crate learns it. |
| `norn-store` | **An SDK for talking to SQL** — DDL, migration machinery, the DDL fingerprint, the four pillars (FTS5, vector, findings, migrations), write-through increments, database-side heal rungs, derivation counters, the read builders (wire params → emitted SQL, not built) with the snapshot read handles they run on, and — behind an off-by-default feature, so a shipped build carries none of it — the arrangements the induced-failure suite reaches a rung or a refusal through from outside. Its verbs translate cleanly to SQL; no business logic beyond how queries are composed. A findings row records the kind it was handed: **finding-kind vocabulary is `norn-wire`'s**, and the store stores it rather than defining it. | The first effect seam. Read builders live here because the `EXPLAIN` gates test the builder's emitted SQL — store schema and queries co-evolve or they drift. |
| `norn-embed` | **Text in → vector out, model identity explicit** — the embedding trait with `(model id, version)` first-class in the API; the deterministic stub is the default build; the real pinned runtime compiles only behind the release/soak feature. Never touches the vault or the database, never decides anything. Its one permitted effect is the opt-in machine-local weight fetch/load, at a path the host injects **from `norn-config`**; fetched weights are integrity-pinned by a static manifest compiled into the crate, mapping `(model id, version)` to a sha256 digest and a source URL. A blob's on-disk name carries its digest, and verification happens at fetch, so an unverified blob never appears under a name anything loads. A fetch failure or a digest mismatch refuses with a structured reason: semantic search stays un-enabled, and nothing else degrades. Acquisition is eager, at the explicit enable act, never lazy inside a query. A model upgrade is a release-time manifest change plus a migration of derived vector state, never ambient upstream drift. | Heavy-dependency isolation (the model runtime stays out of every development build), and a structural guarantee that inference cannot reach findings or plans. |
| `norn-config` | **Machine-local state, one owner** — the machine-local layout (config directory, data directory, the machine-local schemas directory, the weights directory, and the per-vault derived-state directory, keyed by vault name), the registry file read/write, the bearer token read/write, and the loopback endpoint convention. The token file holds a **set of tokens keyed by label**, not one token: rotation is adding a second label and removing the first, and every stored secret is a credential the serving side verifies against. The endpoint is a convention rather than a discovery — a per-channel default port, with an explicit port override the one way past it. Every machine-local path is channel-qualified, and no API takes a channel. Never touches a vault. | The one state surface both sides of the client/host seam must read — the serving side to authenticate, the client to find the host at all. Hand-sharing that convention in two crates is a drift class on a security-relevant file. |
| `norn-host` | **The protocol-blind orchestrator** — registry semantics (the serving set; file access via `norn-config`), vault entries and lazy attach, vault schema (resolved through the registry entry's schema-source — default: a path inside the vault — read via `norn-fs` as one atomic read-and-fingerprint act, and pinned at attach), and the worker pool (the one applier, mutation planners, the repair planner, embedding workers). The pinned schema is projected into the store's meta (bytes, fingerprint, generation) as derived state; the file remains its sole authority. **Wire in, wire out**: a plain library with no sockets, composing `fs` + `text` + `store` + `embed` (+ `config`). Never touches vault bytes, and the one direct effect reserved to it is the one-shot legacy-cache janitor, which is not built. | The composition seam: sole subscriber of filesystem facts, sole caller of store increments, sole executor of plans — reachable only as wire types. |
| `norn-mcp` | **MCP semantics, no transport** — derives tool schemas from `norn-wire`, translates MCP requests to wire params and wire reports to MCP responses. Pure functions, unit-testable like `norn-text`. | The derived-renderings owner for the MCP surface; protocol shape quarantined from both orchestration and plumbing. |
| `norn-serve` | **The HTTP surface** — the serving socket, routing, auth middleware (bearer verification via `norn-config`), and whatever accretes above the protocol later (TLS when a real remote consumer exists, rate limits, request logging). Routes to `norn-mcp`, dispatches wire types to `norn-host`. | The serving effect seam; it keeps HTTP-layer growth out of the orchestrator. |
| `norn-console` | **The CLI presentation kit** — clap *extensions*, never clap re-implementations; render conventions (palette and colors, records display, table/list projection, error-output envelope); input conventions (stdin handling, confirm prompts). **norn-agnostic**: generic over record types via traits, with no dependency on any other workspace crate. | Single source of truth for how output looks and input behaves, plus a standalone future — it is designed for extraction as an independent reusable crate. |
| `norn-client` | **Everything outside the host** — argv → wire params, loopback routing with endpoint discovery and token read via `norn-config`, TTL auto-launch (connect-or-spawn of the same artifact's host entry point, always a separate process), projections of wire reports onto `norn-console` conventions, the stdio-MCP shim (JSON-RPC framing only; the MCP rendering lives in `norn-mcp`), and the machine-local verbs: self-update, service install, `completions`, `manpage`. Leans on `clap` for everything clap can express — derive API, parsing, completions generation, error surfaces. | The always-routed enforcement seam (see boundary invariant 1). |
| `norn` (bin) | **Composition root only** — wires `norn-serve` and `norn-client` into the single shipped artifact. | Distribution: one artifact for self-update, service install, and TTL auto-launch alike. |
| `norn-fixtures` (dev) | **The deterministic vault generator** — the same `(profile, seed)` produces the same tree, byte for byte, up to the filesystem's own filename normalization. Six realism knobs: body-length distribution, ambiguity-class size `k`, link density, directory shape (tree depth and fan-out, root placement, placement skew, leaf-name shape), non-Markdown clutter, symbolic links by species (in-vault file, in-vault directory, dangling, outbound); heading density is a field of body shape, and document count is the profile's own scale. A profile asking for symbolic links **refuses to generate** where one cannot be created — asked of the build before the target is touched, and of the filesystem under the output directory before the tree is written — rather than emitting a tree without them. Self-contained leaf; its writer is its own. See [ADR 0002](decisions/0002-fixture-determinism-and-calibration.md). | Shared by tests, per-PR gates, and the soak lane; never ships. Writing temp trees is its job, so (with testkit) it holds use-site allows for the filesystem rule. |
| `norn-testkit` (dev) | **Assertion helpers and harness scaffolding** — counter, `EXPLAIN`, and size-independence assertions; the churn driver, which is not built; corpus activation gating; regression-registry loading and its dormancy gate; the architecture gate. Suites execute as integration tests in the crates they exercise, and a suite whose subject is workspace-wide data lives with that data in the `norn` bin package: the argv corpus, which additionally needs the built binary cargo only makes reachable there, and the regression registry. | Enforcement machinery lives once: helpers here, suites with the subjects they exercise. |

Two membership rules deserve emphasis because they are the ones most often eroded by
convenience:

- **`norn-client` is deliberately excluded from `init`.** A verb that scaffolds
  configuration inside a vault is making a vault request, so its disposition belongs to
  the **verb charter** — not to the machine-local verb set.
- **`norn-console` extensions are earned case by case.** The bar is doing *more* than clap
  allows, never a preference for our own code. Re-rolling a clap-native capability
  (parsing, completions generation, error surfaces) is a defect. The custom help renderer
  and the short/long help stance are the exemplars, and the class stays open on those
  terms — new exceptions are judged by the verb charter, which owns help doctrine.

### Dependency allowlist

Arrows point at the dependency. Leaves at the bottom; nothing points up. **This graph is the
allowlist** — the complete set of permitted workspace normal-dependency edges.

The allowlist is exhaustive, and that is the contract: **a new edge is a deliberate edit to
this table, never a side effect of adding an import.** An edge that is not here fails the
architecture gate whether or not it points somewhere sensible.

```mermaid
graph TD
  bin["norn (bin) — composition root"] --> serve["norn-serve"]
  bin --> client["norn-client"]
  serve --> mcp["norn-mcp"]
  serve --> host["norn-host"]
  serve --> wire["norn-wire"]
  serve --> config["norn-config"]
  mcp --> wire
  host --> store["norn-store"]
  host --> wire
  host --> text["norn-text"]
  host --> fs["norn-fs"]
  host --> embed["norn-embed"]
  host --> config
  client --> wire
  client --> console["norn-console (norn-agnostic)"]
  client --> config
  store --> wire
  fixtures["norn-fixtures (dev)"]
  testkit["norn-testkit (dev)"] -.-> fixtures
  testkit -.-> wire
  testkit -.-> store
```

Written out, the permitted edges are exactly:

| From | To |
|---|---|
| `norn` (bin) | `norn-serve`, `norn-client` |
| `norn-serve` | `norn-mcp`, `norn-host`, `norn-wire`, `norn-config` |
| `norn-mcp` | `norn-wire` |
| `norn-host` | `norn-store`, `norn-wire`, `norn-text`, `norn-fs`, `norn-embed`, `norn-config` |
| `norn-client` | `norn-wire`, `norn-console`, `norn-config` |
| `norn-store` | `norn-wire` |
| `norn-testkit` (dev) | `norn-fixtures`, `norn-wire`, `norn-store` |
| `norn-wire`, `norn-text`, `norn-fs`, `norn-embed`, `norn-console`, `norn-config`, `norn-fixtures` | *(none)* |

Seven crates are leaves with zero workspace dependencies. `norn-store` depends on
`norn-wire` alone — parsing happens in host orchestration, so the store's API takes typed
facts, never raw documents.

### Boundary invariants

Thirteen invariants. They are not all carried the same way — some by an absent dependency
edge, some by a lint, some by review until a rule can express them. The
[enforcement posture](#enforcement-posture) section describes those three postures; the
authoritative mapping of invariant to mechanism is the harness's code, not this document.

1. **`norn-client` never depends on `norn-store`, `norn-host`, or `norn-serve`.**
   Always-routed is therefore true by construction: no client-side code path can open a
   database or serve in-process. The two crates meet only inside the composition root, and
   the only channel between them is loopback HTTP speaking `norn-wire`. Always-routed's
   scope is **vault requests**; the machine-local verbs (self-update, service lifecycle,
   `completions`, `manpage`) are the named exception class — they touch no vault and no
   database, and the absent edges are what makes that permanent.
2. **`norn-host` drives the substrate; `norn-store` implements it.** These are separate
   claims, about different things:
   - **Driver calls live in `norn-store`.** No other crate's code opens a SQLite connection;
     that is the lint's subject. The harness is no exception: `norn-testkit`'s `EXPLAIN` and
     counter assertions run through `norn-store`'s own API, which exposes what they need, so
     testkit opens no connection itself.
   - **Among product crates, `norn-host` alone links `norn-store` into a running process**,
     so in the shipped artifact one crate reaches the substrate. `norn-testkit` also depends
     on `norn-store`, but it never ships.
   - The host is likewise the plan executor: one applier, per invariant 4.
3. **`norn-wire` has zero workspace dependencies and zero effects.** Nothing crosses the
   client/host seam that is not a wire type — no untyped JSON value, no JSON-in-a-string.
4. **One plan vocabulary, one applier.** Mutation verbs and the repair planner both compile
   to `norn-wire` plan types; the host's single applier executes all of them. A second
   execution path is a defect.
5. **Surface-specific parameters are defects.** `norn-client` renders wire types to flags
   and stdout; `norn-mcp` renders them to tool schemas. Neither defines vocabulary.
   Surface-specific *presentation* is fine.
6. **One render seam, owned by `norn-console`.** Verbs return data; `norn-console` owns the
   mechanisms (help rendering, palette, records display, error envelope); `norn-client`
   owns only the projections of wire types onto those mechanisms. A verb or client module
   that writes stdout or resolves color/tty itself is a defect.
7. **`norn-console` never depends on a workspace crate.** The reverse edge
   (`client → console`) is the only legal contact. This keeps the crate extraction-ready
   and keeps norn semantics out of presentation mechanics.
8. **Two vault-effect seams, and only two.** In the shipped product, `norn-fs` and
   `norn-store` are the only crates that touch a vault — its files and its derived
   database. Derivation is
   `fs.walk → text.parse → store.upsert`; application is
   `text.compose → fs.write_atomic → store.increment`. The seam governs *vault* effects
   specifically, so a crate having some other effect is not a violation of it — serving
   sockets, loopback routing and process spawn, tty, machine-local config-directory state
   and machine-local filesystem writes are each their owning crate's business. The
   filesystem table in the enforcement section maps where effects live; it, not this
   sentence, is the place that list is maintained.
9. **The fs event stream carries filesystem facts only**, from a single producer (the
   watcher). Domain eventing, if it ever earns existence, is a separate host-internal
   concern — never a rider on the fs bus.
10. **One parser.** All document syntax — frontmatter, headings, sections, links (both
    families), tags — is read and written through `norn-text`. A second interpretation of
    document text anywhere else is a defect.
11. **Machine-local state has one owner.** `norn-config` owns config-directory bytes —
    registry file, bearer token, endpoint conventions — and every read and write of them
    flows through its API. Registry *semantics* (the serving set and its mutation verbs)
    stay host-side; config owns shape and storage. The shared dependency is not a channel:
    client–host coordination still flows only over loopback HTTP. Config's API splits along
    that seam — the **registry surface is host-only**, which a crate edge cannot express and
    which therefore carries a named symbol lint, while the machine surface (endpoint
    discovery, token read) is shared. Token *generation* at service install is the client's
    action, executed through that API — the client decides the write happens, `norn-config`
    performs it, and no second byte-writer appears. **The per-registration mechanism files
    are `norn-fs`'s, not config's** — the maintainer lock file under the data root, and the
    shadow home under the data root too unless the vault is on another filesystem, are
    vault-effect mechanisms that follow the seam owning their meaning rather than the directory
    they sit in. Both crates are leaves, so each spells the file mechanics its own protocol
    needs; [the file-mechanics split](#two-spellings-of-the-file-mechanics-and-the-discipline-both-keep)
    records what diverges between the two spellings and what must stay aligned across them.
12. **`norn-embed` is blind.** No `embed → store` and no `embed → fs` edge exists; the host
    mediates every vector write. Semantic search stays a query surface and never a
    correctness input, because the inference crate structurally cannot reach findings or
    plans.
13. **The orchestrator is protocol-blind.** `norn-host` depends on no protocol or serving
    crate; requests reach it only as `norn-wire` types. Dependencies point downward
    (`bin → serve → {mcp, host}`), never the reverse. A protocol type in orchestrator code
    is a defect, and the missing edges make it a compile error.

### Enforcement — the graph is a gated contract, not a convention

The edge set above is asserted by an **architecture gate in `norn-testkit`**: a per-PR test
that reads `cargo metadata` and asserts the workspace **normal-dependency** graph is
*exactly equal* to the allowlist above, restricted to crates currently present and evaluated
under pinned default and all-features selections. When a workspace crate first declares a
target-specific dependency, the matrix also gains a pinned `--filter-platform` selection
for that target. [ADR 0008](decisions/0008-present-crate-dependency-equality.md) records why
exact equality is restricted to crates currently present.

- A forbidden edge fails.
- **A new edge not deliberately added to the allowlist also fails.** Absence from the table
  is a rejection, not a gap to be filled by whatever compiles.
- Development-dependency edges (every crate's tests reaching testkit) are documented but
  ungated: they inherently cycle. They are omitted from the graph above, whose dotted edges
  are testkit's own normal dependencies.
- **A crate declared isolated links nothing under the default selection**, registry crates
  included. Heavy-dependency isolation — the `norn-embed` guard, keeping a model runtime out
  of every development build — is not a property of one manifest: a feature belongs to the
  dependent, so a consumer naming `features = [..]` or forwarding one of its own defaults
  pulls in whatever it reaches. The crate's own manifest test states what it asks for; this
  gate reads what the workspace resolves.
- **A local crate outside the member set fails**, because an excluded crate is outside the
  allowlist, the lint ruleset and every test the workspace runs. One exemption exists, and a
  package earns it by satisfying both halves: its manifest sits under `vendor/`, **and** the
  workspace root manifest replaces a crate of its name through `[patch.crates-io]`. The
  patch table is what makes the directory mean anything — without that half the exemption
  would be a directory any path dependency could be moved into. A package satisfying both is
  one published third-party release with a patch applied, named in `vendor/README.md` with
  the capability the patch adds and what removes it. The edge to it is the third-party edge
  the registry release already carried, so it is not the allowlist's subject — while the
  isolation rule above still reads it, because what a crate links is what it links. A patch
  is temporary by construction: it exists until a published release carries the capability.

Symbol-level rules the dependency graph cannot express **escalate to lint tooling**. Which
tooling expresses them is an implementation detail, not a fixed part of this contract —
clippy's `disallowed-types` and `disallowed-methods` are the starting point, with custom
lints if configuration-level lints prove insufficient. A rule no configuration-level lint
can express yet is review-held: the wire-seam rule is today, because `disallowed-types`
cannot tell a signature crossing the seam from legitimate test and helper use. The rules:

- no `serde_json::Value` crossing the wire seam
- no SQLite connection opened outside `norn-store`
- `std::fs` disallowed workspace-wide
- no `norn-config` registry-surface use outside `norn-host`
- no direct stdout writes outside `norn-console`

Where clippy carries the ruleset, one workspace-root `clippy.toml` holds all of it, because
that configuration file does not merge per-crate. The crate that legitimately owns an effect
carves it out with an explicit `#[allow]` **at the use site**, which doubles as an audit
marker.

Two rules carry a tabled carve-out set. The tables below map where effects legitimately
live, and are kept accurate as effects are added — they are not the enforceable list. **The
enforceable ruleset is the harness's code**; a site missing from a table is a gap in the
table, and a site the ruleset rejects is rejected whatever the table says.

The other two configured rules carve out inside a single crate each, and that fact is the
whole map: the SQLite rule's one carve-out is the connection `norn-store` opens, which is
the seam the rule exists to keep to one place, and the registry rule's are in
`norn-config`'s own suite, which exercises the surface the rule reserves to `norn-host`.

Each effect a row names is one of two things. Most are **carried today** by a use-site allow
in a crate the workspace holds. The rest are **reserved** — the crate is not written yet, or
the effect inside it is not — and the row says so beside them; a row may well name some of
each. A reserved effect is as binding as a carried one, for the reason an absent crate's
membership rule binds.

**Filesystem (`std::fs`)** — effect sites:

| Site | Effect |
|---|---|
| `norn-fs` | The vault filesystem seam itself: walk, read, stat, atomic write, flock. Plus the per-registration mechanism files it owns, keyed by data base, channel and vault name — the maintainer lock file under the norn data root (created once, never unlinked) and the shadow home (created, staged into, swept), which is under the data root too unless the vault is on another filesystem. This is the rule's home, not a carve-out. Its own suites carry allows besides, for the trees and states a case arranges and judges |
| `norn-store` | The parts of the derived database's file lifecycle the driver does not cover: deleting the database at heal rung 3, tearing down the throwaway store behind disposable derivation, and preparing its parent directory. Its suites carry allows too — the directory a case's store lives in, the bytes a case damages, and the sidecar a torn changeset left |
| `norn-config` | Config-directory bytes, and the whole protocol around them: the registry file and the token file, the `0700` creation of the config directory, the `.lock` file each data file is guarded by, the temporary file every write lands in and the sweep that clears a dead writer's, and the directory fsync that makes a rename durable. Its suites carry allows for what they arrange outside that protocol: modes the API never writes, links planted at a name, and residue a dead writer left |
| `norn-embed` | *Reserved.* The opt-in weight fetch and load, which is not built; the crate reaches no filesystem today |
| `norn-host` | Its suites' scaffolding, which is the whole of the crate's contact today: generated trees, subprocess probes, and fixtures impersonating external editors and retargets. *Reserved* beside it: the one-shot legacy-cache janitor, which is not built |
| `norn-client` | *Reserved.* Machine-local effects only: self-update binary replacement and service-unit install |
| `norn-fixtures`, `norn-testkit` (dev) | Temp trees and harness scaffolding: generated and scratch trees, the sandbox a process case runs in and the artifact installed into it, this crate's own cross-process lease file, the run's output files and the job-summary file a workflow names, and the workspace's own files a gate is the reader of — `clippy.toml`, this document, the regression registry and the corpus data |

Two notes on that table:

- **Most of `norn-store`'s disk contact is driver-mediated.** Queries and increments reach
  disk through SQLite; so does creating the file (SQLite creates on open) and rebuilding it
  (that is DDL). What is left over — removing a file, tearing a throwaway store down,
  preparing a parent directory — is where the crate's `std::fs` allows sit. Fixing the
  precise allow-set is the harness's job, not this table's. The lifecycle sits in
  `norn-store` rather than `norn-fs` because the database is store's to own: rung 3 is a
  database-side rung, and the host chooses the mode while store performs the operation.
- **The dev crates are not outside the rule.** One workspace-root `clippy.toml` binds
  workspace-wide and does not merge per-crate, so `norn-fixtures` and `norn-testkit` write
  temp trees under ordinary use-site `#[allow]`s. That is precisely why they appear in the
  table rather than in a sentence exempting them.

`norn-serve` is a deliberate non-entry: it binds a loopback socket, which is not a
filesystem effect.

**Stdout** — write sites:

| Site | Why |
|---|---|
| `norn-console` | *Reserved.* The render seam; this is the rule's home once the crate is written, not a carve-out |
| `norn-client` stdio-MCP shim | *Reserved.* JSON-RPC frames are the protocol; they cannot route through a record renderer |
| `norn-client` `completions` and `manpage` | *Reserved.* Generated artifacts consumed by other programs, not rendered records |
| `norn-fixtures` (dev) | Its command line reports what it generated and what it measured. Routing a dev generator's output through the product's render seam would give a leaf a dependency the allowlist forbids |
| `norn-fs` and `norn-host` measurement harnesses (test targets) | A probe child reports its readings on stdout and the parent test case reads them. The stream is machine-consumed and its shape is the harness's protocol, so a render seam would be reading it, not writing it |

Everything a person reads as *output* still goes through `norn-console`. The carve-outs
cover machine-consumed byte streams and the dev crates' own command lines and harness
protocols only.

#### Two spellings of the file mechanics, and the discipline both keep

`norn-fs` and `norn-config` each own their own spelling of the same three idioms: an
exclusive `flock` with a bounded ABA recheck, a temporary-file/fsync/rename durability
sequence, and a `(device, inode)` stat-identity comparison. **The split is purposeful and
recorded rather than extracted**, and invariant 11 is why it has to be: both crates are
leaves the rest of the workspace depends on, so neither may depend on the other, and a
third crate holding the mechanics would be edges the allowlist does not carry — added for
code that looks alike rather than for a boundary that means something.

What the two spellings say is not the same thing:

- **Waiting.** `norn-fs` never blocks. Acquisition answers `Contended` and carries the
  incumbent's diagnostics, because the caller is a host deciding whether it maintains a
  vault. `norn-config` waits, without a bound, because its caller is inside a
  read-modify-write of a small machine-local file and the wait is one other writer's
  rewrite.
- **Where a write is staged.** `norn-fs` stages in the shadow home under the data root
  ([ADR 0011](decisions/0011-shadows-live-outside-the-vault.md)), or — when the vault is
  on another filesystem, per invariant 11 — under `.norn/tmp` inside the vault, which the
  walk and the watcher exclude by that one root whatever key's home sits beneath it.
  Either way an unpublished write is never a file the watcher reports on, and the
  exclusion is what carries that in the fallback placement rather than the location.
  `norn-config` stages a sibling temporary in the config directory and sweeps a dead
  writer's residue under the same lock that guards the write.
- **What a failure is called.** `Refusal` and `ConfigError` are separate closed
  vocabularies over separate subjects: vault documents a plan was composed against, and
  machine-local state one writer owns. A refusal never reports a published replacement; a
  config error distinguishes a write that did not happen from one whose rename landed and
  whose durability was not confirmed.
- **Fault injection.** `norn-fs` threads a fault seam through its write stages, because
  the crash windows it claims cannot be produced on a temporary directory. `norn-config`
  carries no such seam.

What must stay aligned is the discipline, and a change to either spelling is judged
against it:

- **A link at a name is refused, never followed.** On the lock-file open a symlink
  planted at the lock name is refused rather than followed to a file nothing else guards,
  and the same discipline binds the anchored read seam — every document and schema
  `norn-fs` reads for content: the read is anchored at a directory and reaches the name
  below it one component at a time, with `O_NOFOLLOW` on each, so a link anywhere in the
  path ends the read instead of redirecting it. The anchor is the boundary rather than a
  name inside it, so it alone is resolved exactly as spelled. The mutation verbs'
  precondition open is not yet anchored: it holds the link and regular-file rule at the
  final name only, and its ancestor components are resolved by the kernel. What an operator sees: a vault whose `.norn` or
  `.norn/schema.yaml` is a symlink does not attach, a configured `schema_source` that
  names a symlink does not either, and the refusal names the component that stopped the
  read. One schema shared across vaults is spelled as a `schema_source` naming the file
  itself.
- **The handle-versus-name identity recheck.** An advisory lock follows the file, so
  after it is taken the handle's `(device, inode)` is compared against what the name
  resolves to now; a mismatch drops the handle and takes the lock again, bounded by a
  retry count whose exhaustion is reported rather than looped on. **A stat that fails for
  any reason other than absence is that machine failure, reported as itself** — never a
  spent retry, and never an answer of "this name means a different file". That holds
  wherever the comparison is made, including `norn-fs`'s post-acquisition health check on
  a maintainership it already holds.
- **The publish order:** create the file with `create_new`, write, fsync the file, rename
  onto the destination, fsync the parent directory. The rename is the atom every reader
  is protected by, and the parent's fsync is what makes it survive a power cut. An
  exclusive create is the one shorter case — `create_new` claims the name atomically, so
  there is no rename and nothing staged — and the bytes still reach the disk before the
  parent does.

The one place the two deliberately part on that last point is **what a failed parent
fsync is told to the caller**. Both hold the same stance — the rename landed, every
reader sees the change, and this is never reported as a write that did not happen — and
they surface it differently. `norn-config` names it as its own outcome, because a
machine-local file has one writer and a caller that reads it back. `norn-fs` leaves it
best-effort and silent, because no refusal of its reaches a parent fsync at all, so
reporting would mean growing the success side instead — at four positions, since a move
fsyncs the destination's parent and the source's and either can fail alone — and because
a vault is multi-writer: whatever is finally at the path is what the watcher reports and
what derived state converges on.

### Enforcement posture

Three postures are in play, and which one carries a given invariant changes as the lint set
grows:

- **The dependency graph is gated.** The architecture gate asserts exact equality against
  the allowlist, so an edge that would let a violation compile cannot be added silently.
- **Symbol-level rules escalate to lints**, adopted one at a time as each invariant becomes
  real.
- **Invariants no rule can yet express are review-held** until one can. That is a real gap,
  not a formality — a review-held invariant rots quietly.

**This document carries no authoritative per-invariant ledger.** Where an invariant's own
statement names what carries it — absent edges in 1, 12 and 13, the SQLite lint in 2, the
filesystem table in 8, the named symbol lint in 11 — that statement stands and is
load-bearing. What does not exist here is the complete mapping: **the authoritative mapping
is the harness's code, not this document.** Enforcement mechanics become true by being
executable, and a prose ledger of them here would be a second, unexecuted spelling of that
ruleset — drifting from the code the moment either moved. See [ADR
0003](decisions/0003-boundary-enforcement-harness.md).

---

## Part III — Runtime

### Topology

**One host process serves all registered vaults**, with two lifecycles: a supervised
service, or an auto-launched TTL instance (supervisor off, TTL on). Same binary, same attach
seam — the one described in [the heal ladder](#2-the-heal-ladder).

The registry is the host's serving set. Registration gates durability — durable database,
watcher, warm trust — while unregistered roots get disposable derivation over a throwaway
store. That second half is not built: the attach seam refuses a throwaway demand, and only
registered vaults attach. Lazy attach bounds file-descriptor and watch usage against a
measured budget, not an assumed one.

```mermaid
graph LR
  subgraph callers["Callers"]
    cli["norn CLI"]
    shim["stdio-MCP shim"]
    agents["HTTP MCP clients"]
  end
  cli -- "loopback HTTP + bearer" --> api
  shim -- "loopback HTTP + bearer" --> api
  agents -- "loopback HTTP + bearer" --> api
  subgraph hostp["host process — supervised service or TTL instance"]
    api["norn-serve — HTTP · auth · MCP (norn-mcp)"] --> reg["registry (serving set, norn-host)"]
    reg --> v1
    subgraph v1["vault entry (per registered vault, lazy attach)"]
      watcher["watcher (norn-fs facts)"] --> orch["host orchestration — scoped increments (via norn-store)"]
      workers["workers — applier · planners · embeddings"] --> orch
      orch --> db[("SQLite — FTS5 · vector · findings · migrations")]
      reads["read builders (norn-store)"] --> db
    end
  end
  watcher --- files[("vault files — all access via norn-fs")]
  workers --- files
```

Placement notes:

- The flock primitive lives in `norn-fs`; the host applies it lazily
  per entry, **flock-then-attach**, over the entry's own derived state. A
  contended entry refuses without preventing the process from serving unrelated
  registry entries. The channel-scoped listening socket is `norn-serve`'s
  independent process-level concern; an entry's maintainer lock never gates it.
- Disposable derivation for unregistered roots is a host attach mode over a throwaway store.
  It is not built; the attach seam refuses a demand that names that mode.
- The first-run janitor that clears orphaned legacy cache directories is a host startup task
  over machine-local paths. It is not built; nothing sweeps those directories today.
- The endpoint and bearer conventions on both ends of the loopback edges come from
  `norn-config`.
- **A build has a channel identity, and it is what keeps two installations apart.** The
  channel is fixed at compile time by `norn-config`'s `channel-live` feature and reaches
  everything two ways: the app-directory name, so a development build and a released build
  have two config directories, two data directories, two registries and two token files;
  and the default port, so their hosts listen on two sockets and coexist on one machine. A
  dev build cannot reach live state through a path, because no path it can build names
  one; the port is a default rather than a wall, since an explicit port override may name
  any socket. **The release artifact must be built with `channel-live` enabled** — a
  released binary built without it serves the development tree under the development port,
  which is the failure this separation exists to prevent and the one place it depends on
  the build rather than on the code.
- Auth binds loopback with a static bearer token generated at service install, and stays
  middleware-shaped. TLS, multi-user, and remote access are explicitly out of scope until a
  real remote consumer exists.

### Request flows

**Read — SQL-native.** Params arrive through `norn-serve` as wire types (protocol shape is
shed at the `norn-mcp` boundary); the host resolves the vault entry; a `norn-store` builder
emits SQL; SQLite answers; rows become a wire report. No repository tier, no domain-object
hydration. Warm requests assert zero derivation counters.

**The snapshot-read seam is carved, and production mints no reader through it.**
`norn-store`'s reader type is uninhabited and the production attachment answers no handle,
so no read reaches a database today; what the seam states is carried by the entry's moves
and by the ops the suites install, and it is this. Reads reach the database independently of
orchestration, on a read-only snapshot handle `norn-store` mints from the live `Store` —
held in entry state beside the attachment, never inside it, so a read proceeds while a warm
lifecycle job holds the store. The snapshot is established under the entry gate lock in the
same critical section that reads trust — established by a read statement, since a bare
deferred `BEGIN` takes no snapshot — so the trust label and the snapshot describe the same
instant, and the read runs outside the lock: it sees the last committed increment, never
blocks the writer (checkpointing stays passive — an aggressive checkpoint mode would trade
that guarantee away), and may trail in-flight derivation. Concurrent reads serialize against
each other on the one reader per entry; measured reader contention is what mints more
through the same seam. The reader is torn down before the store closes on every closing
path, and an in-flight read pins the entry — which buys the read deferral alone: **no idle
detach is scheduled while a read is running.** It buys no more than that. A refusal,
a host destruction, a detach already scheduled when the read began, and a job leg failing
its way into a release each reach the entry without reading a pin, and a read in flight
stops none of them. Through such a teardown the read keeps answering from the handle it
holds, and that a database file outlives the teardown for it is carried by nothing: no move
states it, and it is read-path residue rather than a contract this document states. [ADR
0015](decisions/0015-snapshot-reader-lifetime.md) records the rationale and the priced costs.

The suffix-resolution ladder follows the same split. Targets resolve by **right-to-left,
segment-aligned path suffix** — `glossary` matches any `**/glossary.md`; `norn/glossary`
matches only `**/norn/glossary.md`; stem resolution is the one-segment case. This is *the*
grammar across every target surface: CLI, MCP, wikilinks, filters. It is expressed as
`norn-wire` resolution types; `norn-text` parses link *syntax* only; execution is a store
builder with its own `EXPLAIN` bar and index support. Candidates emit as minimal
disambiguating suffixes, and the findings pillar indexes full candidate enumeration so the
wire's bounded head stays a head rather than becoming the query surface.

The links table stores **syntactic facts only** — raw target, family, protocol, title, span —
and resolution runs at query time through this one grammar; resolved edges are never stored,
and a materialized projection could only ever arrive as keyed, invalidated derived state.
Backlinks are an indexed suffix join over those facts. How a target resolves **derives from
the fact, protocol first and family second**: a written `protocol://` prefix shadows the
family, because the family says which grammar the author wrote and the protocol says what
the address is. A protocol-free wikilink target resolves as a suffix address, while a
protocol-free inline Markdown target resolves as a relative filesystem path against the
containing document — vault-root-relative when the path is rooted, and containment-bounded
either way.

**Derivation — attach, heal, watch.** Pure host composition. Cold attach walks
`fs.walk → text.parse → store.upsert`. Warm operation is watcher facts from `norn-fs`
driving scoped `norn-store` increments; the heal ladder splits across the same two seams
(see [the heal ladder](#2-the-heal-ladder)).

Full-text search is maintained transactionally inside the increment. Embeddings are
eventually consistent via async workers, and the surface says so. Vectors are versioned,
derived, rebuildable state — pure functions of (model id, version, content) — so a model
upgrade is a migration of derived state.

**Vault configuration is two files, split by whether it shapes derivation.** The schema file
carries everything derivation- and validation-shaping, exclusion paths included, and its
fingerprint is the invalidation key for the schema-dependent derived tables. The operational
config file carries the rest, and a change to it may never require re-derivation. Every
schema change converges on one path — re-read, atomic re-pin, re-derivation of the
schema-keyed tables — whether it arrives as a hand edit the watcher observes or through a
future verb, and it invalidates nothing outside those tables. The re-pin discards the
schema-keyed rows and names no paths, so the re-derivation that records them again is the
vault-wide walk. That walk is hash-authoritative: it reads every markdown, non-excluded path
no document row stands at — which is every path a **place-scoped** finding stands at whose
subject the vault still holds — and beyond the schema-keyed rows it re-derives only a
document that drifted since it was last derived, which any reconcile owed anyway. Two kinds
of finding are outside what it files again: one whose path left the vault, which is why the
re-pin's discard is where such a finding ends, and a **document-scoped** one standing at a
path whose document did not drift, which returns when that document next changes.

**Mutation — everything is a plan.**

```mermaid
sequenceDiagram
  participant C as norn-client
  participant S as norn-serve
  participant H as norn-host
  participant W as worker (applier)
  participant F as vault files (via norn-fs)
  participant D as SQLite (via norn-store)
  C->>S: HTTP (bearer; protocol shape)
  S->>H: verb params (wire)
  H->>H: compile to typed plan (wire vocabulary)
  H->>W: plan
  W->>F: fingerprint → shadow → verify → swap (protocol owned by norn-fs)
  Note over W,F: drift detected → refuse-and-refresh (fresh forecast, hash-CAS confirm)
  W->>D: write-through increment, scoped to blast radius
  Note over W,D: mark-invariant — the same counters under either mark
  W-->>H: outcome
  H-->>S: report (wire)
  S-->>C: HTTP response
```

`apply` is the same flow entered with an externally supplied plan. Repair is a planner over
the findings table feeding the identical applier; its plans cite the finding generation they
were planned against, and repair reads the live ambiguity class rather than a finding's
snapshot.

Two contracts inside that flow carry weight:

- **Write-through.** The worker composed the post-state, so the increment writes it —
  database updates scoped to the blast radius, composing supplied facts and re-deriving
  nothing. The bar is **mark-invariance**: the same changeset reads the same derivation
  counters whether it is marked derived or composed. The one counter that names a
  computation is the canonical-JSON projection of supplied frontmatter, which is storage
  encoding rather than recomputation and runs the identical code path under both marks — so
  the bar binds on the counters that could differ.
- **Refuse-and-refresh.** Detected drift refuses and returns a fresh forecast whose content
  hash rides the confirmation as an implicit compare-and-swap. Auto-rebase on drift is
  deliberately rejected: a changed world deserves a re-plan.

---

## Where the contracts live

- **This document** — the invariant spine, the crate map and its membership rules, the
  dependency allowlist, the boundary invariants, and the runtime topology and flows.
- [`decisions/`](decisions/) — qualifying durable decisions and their rationale, indexed in
  [`decisions/README.md`](decisions/README.md). Admission and lifecycle are governed by
  [`AGENTS.md`](../AGENTS.md); task evidence and current system contracts live elsewhere.
- [`glossary.md`](glossary.md) — canonical project-specific language, organized by concept
  rather than by the task, ADR, or contract that introduced it.
- `.github/workflows/` — the two CI lanes: counters gate per PR, clocks trend in soak. The
  job comments are the in-repo marker for which gate is filled and which is still a
  placeholder.
