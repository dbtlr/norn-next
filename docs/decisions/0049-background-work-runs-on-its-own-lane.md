---
status: proposed
date: 2026-11-02
---

# 0049 — Background work runs on its own lane and starts when it is owed, and only a bounded heal slice holds a vault's claim

Background work gets its own threads in `norn-host`, the **background lane**. Every unit of
work no request waits on runs there: an engine's initialization and feed consumption, the
store's integrity verification, the rolling heal and the shadow sweep. The shared worker
pool keeps only correctness work and readiness work. A semantic turn and a verification
hold no vault claim, so neither delays that vault's writes. The rolling heal and the shadow
sweep still take the claim, for one slice bounded by work and by time. The lane starts work
when a wake or a deadline says it is owed, never on a poll tick.

This decision replaces two passages of
[ADR 0048](0048-a-build-step-ends-at-its-closing-take.md) and restates each one, amended,
below: the semantic scheduling and install rules of its section *Whole-vault and semantic
readiness*, and the *Known limit* of its section *A read waits for the facts it met*. Every
other rule of ADR 0048 stands as written.

## Context

Background work today runs as a maintenance job on the shared worker pool, under the
vault's claim, holding the vault's attachment. A dispatcher poll tick or the end of a leg
schedules it. Three costs follow from that placement.

- **A semantic slice holds the store writer.** The feed-read handle borrows the
  attachment's `Store` exclusively, so a drain page runs under the claim, and every write to
  that vault waits behind it. With the stub embedder, a page costs microseconds. With a real
  model, one page of up to 256 embeddings costs seconds, and each one is time the vault
  cannot take an apply.
- **Semantic work never converges under continuous edits.** A slice runs only on a quiet
  leg. It yields to owed heal rungs, queued applies and pending watcher facts. A vault that
  someone keeps editing is never quiet, so its embeddings fall further behind for as long
  as the edits continue.
- **Verification holds the attachment for a full scan.** `PRAGMA integrity_check` reads
  every page of the database. While it runs, the entry's coverage is out with the leg, so no
  watcher batch drains and every write waits. The scan's cost grows with the database, once
  an hour per attachment.

Start-up has a cost too. Work becomes owed at a commit, but it starts at the next poll tick
that finds it due, so its latency follows the tick interval rather than the work.

## Decision

### The lane serves derived stores in turn

The background lane is a set of threads in `norn-host`, sized by the production policy
value `ProductionPolicy.background_threads` (default 1). The lane holds the one host-wide
background queue: a coalescing set of owed derived stores. A derived store, not a vault
name or a root, is the key, as it is for the read budget, because two registrations over
one root have two stores under [ADR 0016](0016-maintainer-singleton-per-derived-store.md).
The host-wide background budget is the lane's thread count. No background unit runs
anywhere else, and no worker-pool job carries one.

The background lane is not a derivation lane. It runs lane-2 work (engine consumption) and
lane-1 work (the rolling heal's derivation and verification of the lane-1 store) alike. The
lane adds no crate and no dependency edge.

A thread takes one derived store from the set and runs one **turn** for it: units of that
store's owed work until the turn budget runs out or nothing is owed. A store that still
owes work when its turn ends goes to the back of the set. One store with a large backlog
therefore takes one turn while every other owed store takes one, and it cannot starve them.
At most one turn runs for a derived store at a time.

The shared worker pool keeps correctness work (reconciles, applies and heals) and
readiness work (attach, the from-zero build, recovery, rebuild, detach and reload). The
per-entry claim keeps ordering that lane-1 work exactly as it does now.

### A turn is bounded by time, with one unit as its floor

`ProductionPolicy.background_turn_budget` is the time one turn may take. The implementation
sets its default by measurement and records the measured value beside it. The expected range
is 100 ms to 250 ms. A turn checks the budget only between units, and it always runs at
least one unit. A turn therefore ends within its budget plus one unit.

A unit is the smallest step that is not interrupted:

- for an engine, its initialization, or one feed page or reconcile page of its drain;
- for verification, the whole integrity check;
- for a claim-bound slice, one step of that slice, such as one entry of the heal merge.

One embedding is never interrupted, and neither is one page. The engine's page size is
therefore what bounds a turn's overrun.

A claim-bound slice keeps its existing work bounds: 64 enumeration acts, 64 checked
entries and 1 MiB of hashed content. It also takes the turn budget. It ends at whichever
bound it reaches first.

A work bound alone does not fit a lane whose unit cost depends on the embedder. A bound
sized for the stub gives seconds-long turns under a real model. A bound sized for a real
model makes a stub turn mostly scheduling overhead. A time bound follows the actual cost,
and the floor of one unit means every turn makes progress.

### Work starts at a wake or a deadline, with no poll backstop

The lane sleeps until a wake arrives or the earliest background deadline comes. The
deadlines are:

- a rolling heal due, or a rolling heal pass still in progress;
- a store verification due;
- a shadow sweep's age threshold reached.

A wake is contentless. It says that something may be owed, and the owed flags and due
clocks say what. These sites send one:

- a commit that leaves an engine owing work;
- the release of a claim while a claim-bound slice waits for it;
- an engine that becomes ready to initialize, which is the publication that commits its
  delivery;
- an attach.

The dispatcher's poll tick stops starting background work, and so does the end of a leg. A
leg that used to schedule maintenance wakes the lane instead. The wake is host-internal and
never travels on the filesystem event bus (invariant 9). The owed flags and due clocks stay
authoritative, so a lost wake costs latency and never correctness, as
[ADR 0047](0047-a-write-re-decides-only-the-links-whose-inputs-it-moved.md) rules for the
feed. The latency lasts until the next wake from any site or the lane's next deadline.

A detached vault gets no background work until its next attach. Outstanding background
work holds no demand and never defers an idle detach, as ADR 0048 already rules.

### Semantic work leaves the claim

A turn reads the change feed through its own read connection, never through the
attachment's `Store`. It embeds while holding no store, and it commits to its own sidecar.
It takes no claim and no attachment, so it runs alongside that vault's reconciles and
applies. They share only CPU, which the lane's thread count caps. Embeddings therefore
converge while edits continue.

Correctness rests on mechanisms that already exist. Cursors are consumer-owned and valid
within one store epoch, and an epoch change means a rescan (ADR 0047). The drain's
generation check already keeps a hash from being written beside values it does not
describe, even against a writer interleaving through another store handle. The feed-read
handle keeps its surface. Only its source changes, so `norn-semantic` still reads lane-1
state through that handle alone (invariant 12).

**The feed connection belongs to the engine's delivery.** `norn-store` mints the feed
source from the live `Store` in the gate hold that commits the delivery beside a store
lifetime, the same hold that records the delivery's published store today. Minting opens
no database and runs no SQL, as a reader pool's mint does. The connection opens lazily on
the lane, read-only, and an establishment against another store epoch is refused. Each
page is one WAL snapshot transaction, which never blocks the writer. The connection is
outside the request read budget, beside the separate bounds that ADR 0048 keeps for source
writers, private apply readers and sidecars. There is at most one feed connection for each
delivered engine. It closes with its delivery, or when a later publication commits another
store lifetime.

Two other sources were weighed:

- **The request read budget.** A turn holding a budget connection makes a read wait for
  capacity, which is the delay this decision removes. It would also put background turns
  into the read fairness queue.
- **A fresh connection for each turn.** This is correct, but it pays a cold open and a
  cold page cache on every turn. Under continuous edits that cost multiplies with the
  number of turns. A delivery-owned connection costs one connection for each enabled
  engine, the same order as the sidecar connection the engine already holds.

**An engine runs only over a store whose from-zero build has ended.** No sidecar opens and
no turn feeds an engine while its store is under a from-zero build, a rebuild included.
After that build reaches `Ready`, a later heal's label does not pause the engine. A
reconcile's `Healing` label means the vault is taking in a change, not that its store is
unfit to feed from.

**A turn never delays a leg's publication.** It holds nothing a publication takes. ADR
0048's rule that no semantic slice runs between a lane-1 commit and the publication of its
`derived_through` or `Ready` is withdrawn, because that rule guarded a slice that held the
claim. Every leg that commits lane-1 work still records the store reading and the owed
flag before its publication, so a status taken before the turn runs reports the engine
trailing.

**A search waits behind at most one unit.** A turn takes the engine slot's lock for one
unit at a time, never for the whole turn, so a vector rung answers between units.

### A result installs only while its delivery and store lifetime are current

This replaces ADR 0048's rule that an initialization installs only for the claim it began
under. A turn holds no claim, so the claim cannot be the test.

An initialization installs its engine only where the engine map still holds the very slot
it opened for, and that slot's published store lifetime is the one the initialization
read. The slot's identity is its delivery's configuration identity. A disable, a reload
that replaces the section, a detach followed by a re-attach, and a store replacement each
leave another slot, another store lifetime, or no slot, so each refuses a late result.
Resources a refused result holds close after the check, outside the entry gate and the
engine locks.

A turn checks currency before each unit and stops at the first unit boundary after its
delivery or store lifetime retires. Its sidecar commits are the engine's own durable
progress, and they stand: rows are content-addressed and cursors carry their store epoch,
so a commit over a retired store lifetime becomes a rescan for the next turn, never a
wrong answer. A report the turn writes into a retired slot reaches no reader of a current
delivery.

Detach, disable, replacement and store replacement each retire under the engine map's lock
and return. None of them waits for an in-flight unit. The retired engine's sidecar
connection and its feed connection close when that unit ends. Three rules bound what is
left running:

- **The store goes back without waiting.** A detach releases the store writer and the
  maintainer lock while a turn's feed connection can still be open. That connection
  finishes its page against its snapshot and closes, as a wire read does under ADR 0048's
  rule that teardown never waits for a reader.
- **One engine works one sidecar at a time.** A later initialization over the same
  sidecar is a unit of the same derived store, so it starts only after the retired
  engine's in-flight unit ends.
- **A discard waits for at most one unit.** A retirement that deletes the sidecar file
  waits for the in-flight unit to end before it removes the file. SQLite does not keep a
  database sound when its file is unlinked under an open connection.

Host shutdown starts no new unit and waits for no more than the unit each lane thread has
in flight. An unwind out of a unit is caught at the lane's seam, and the lane thread
outlives it. The engine slot reads the unwind as an unwinding drain does today: an engine
borrowed for a page stays in its slot, and an engine taken out for its sidecar rebuild is
abandoned and reads as self-disabled.

### Verification leaves the claim

`PRAGMA integrity_check` runs as one lane unit, on its own read connection, over one WAL
snapshot. It holds no claim and no attachment, so it never blocks the writer, and watcher
batches keep draining while it runs. `norn-store` mints its source from the live store, as
it mints the feed source. The connection opens for the check, sits outside the request read
budget, and closes when the check ends.

A damage verdict enters the damage judgment that ADR 0048 gives damage a read meets. That
judgment schedules the existing rebuild as correctness work through the claim. A verdict
counts only for the store epoch it read, so a verdict over a store that was replaced during
the check schedules nothing. An environmental failure, such as an open failure, a busy
database or descriptor exhaustion, is not a verdict. It schedules no rebuild, and the check
next comes due at its normal interval.

The check is one indivisible unit. It may hold a lane thread for its whole duration, once
an hour per attachment. While it runs, the WAL cannot checkpoint past its snapshot. Both
costs are accepted.

### The rolling heal and the shadow sweep keep the claim

The rolling heal and the shadow sweep are **claim-bound slices**. A lane thread runs each
one holding the vault's claim and its attachment, as the maintenance leg does today. Each
slice waits while the entry owes a heal rung, an apply is queued, or watcher facts are
pending. For the rolling heal, those are its existing waits. For the shadow sweep, the wait
behind pending facts is new, and the cost is small: shadow residue is inert, and a sweep
that runs later removes the same files.

A thread never blocks on a busy claim. A claim-bound slice that finds the claim held marks
its store as waiting for the claim, and the thread moves on to other work. A store's
claim-free units can run in that same turn. The claim's release wakes the lane. When the
claim is free, a store's turn runs its claim-bound slice before its claim-free units:
lexical correctness first, as today.

### Status reports embedding coverage

`EngineStatus::On` gains `coverage: { embedded, total }` beside `freshness`. Both counts
are taken at the feed reading of the engine's last turn and written into the slot's report
as the turn ends, so a status never waits behind a turn. `coverage` is `null` until a turn
has reported, as `freshness` is.

- `total` counts the documents the engine would embed. The names-only rows of unreadable
  documents are not among them.
- `embedded` counts the documents whose current content has embeddings in the sidecar.

`freshness` stays the lag signal. The two answer different questions. A vault whose every
document is embedded, with one document edited 100 times since, reads as full coverage and
100 generations trailing. Search answers keep carrying `freshness` alone.

The counts follow the performance posture. A turn's cost follows the rows it moved, so a
vault-wide comparison on every turn is a defect. The implementation decides how it keeps
the counts. The new field is a breaking `norn-wire` change, accepted before 1.0.

### Known limit

This restates the known limit of ADR 0048's section *A read waits for the facts it met*,
amended.

While a claim-bound slice holds the entry, the entry stays `Ready` and does not yet see an
edit made moments before it. A read in that window can miss that edit. The window is one
claim-bound slice, ended by its work bounds or the turn budget, whichever it reaches first,
plus the one step in flight when it reaches the bound. Watcher batches drain before another
slice starts. A semantic turn and a verification hold no claim and open no such window.

### Non-goals

- No real-model embedder. The stub stays the only one.
- No `norn-config` home for `background_threads` or `background_turn_budget`. Layer 6
  serving gives them one, as it does for the read budget.
- No change to how the per-entry claim orders lane-1 work.
- Reads stay unpreempted.

## Considered options

- **Keep semantic work on the claim.** A real-model page blocks that vault's writes for
  its whole duration. It also widens the window in which a `Ready` vault does not see a
  recent edit to a page of embeddings. Rejected.
- **Cap background jobs inside the shared worker pool.** A cap still lets a background job
  occupy a slot that an attach or a reconcile of another vault needs. Background work keeps
  taking correctness capacity, only less of it. Separate threads isolate the two by
  construction. Rejected.
- **Let a turn run only while its vault is quiet.** This is today's rule for semantic
  slices. Under continuous edits the vault is never quiet, so the engine never catches up,
  and nothing guarantees it ever will. Rejected.
- **Keep a poll tick as a backstop beside the wakes.** A backstop is a second path that
  hides a missing wake site. Tests keep passing while a site is missing, and the only
  symptom is latency up to the tick interval. Without it, a missing wake site shows up as
  owed work that does not start, which a test can see. Rejected.
- **Bound a turn by work alone.** The cost of a unit depends on the embedder, so a work
  bound is wrong for one embedder or the other. Rejected.
- **Replace `freshness` with `coverage`.** Coverage does not measure lag, and search
  answers and the unfiltered vector rung's margin read lag. Rejected.

## Consequences

The implementation lands in four sequential pull requests:

1. the lane and its triggers;
2. semantic work off the claim;
3. verification off the claim;
4. embedding coverage in status.

This decision merges as `Proposed` with the first and becomes `Accepted` with the last. On
acceptance, ADR 0048's status records that this decision supersedes its semantic
scheduling, its install rule and its *Known limit*, and links here. Its content does not
change, and its other rules stay load-bearing.

These consequences are testable:

- No semantic turn runs before `Ready`.
- Owed work starts without a poll tick.
- A turn ends within its budget plus one unit.
- One vault with a large backlog cannot starve another vault's turns.
- An apply completes while its vault's turn is in flight.
- A write completes while its vault's integrity check runs.
- `coverage` equals the true counts.
- A settled drain equals a from-zero recompute.

These costs are accepted:

- Background work and correctness work compete for CPU. The lane's thread count bounds
  that competition.
- With the default of one thread, a verification delays every other derived store's turns
  for the length of one integrity check.
- Each enabled engine holds one feed connection outside the request read budget.
- A turn's overrun is one page. The real-model runtime will have to size the page so that
  one page stays within human scale.
