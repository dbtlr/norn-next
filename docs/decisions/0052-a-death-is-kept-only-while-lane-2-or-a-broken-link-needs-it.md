---
status: accepted
date: 2026-11-09
---

# 0052 — A death is kept only while lane 2 or a broken link needs it

A **death** is a tombstone: the row a document's removal leaves in the store, holding the
path, the content hash it was last derived at, the provenance of the news and the write
generation it was recorded at. Until this decision a death was removed only when its path
derived again, so the table grew with every distinct path the vault ever deleted. A death
is now kept while the vault's lane-2 engine or its census has not yet read it in the death
feed, or while a link that is broken now names its path. A claim-bound maintenance unit
prunes every other death. The store records the highest generation it pruned as the
**death floor**, and a death-feed consumer that has not read through the floor reconciles
against current state instead of seeking forward, as it does after a store epoch change.

## Context

Deaths cost space and verification time, and nothing bounded either. On a release build at
10,000 live documents, each death costs about 670 bytes, two thirds of them index entries.
At 100,000 deleted paths, deaths are 78% of the store, and the periodic verification takes
1.7 s, against 0.5 s with no deaths. At 1,000,000 deleted paths it takes 17.3 s. The
`PRAGMA integrity_check` over the death b-trees dominates, and its cost grows faster than
linearly once those b-trees outgrow the page cache. Every request path stays flat as deaths
accumulate: writes, move-following's bounded suffix read, change-feed pages, the search
margin and store open. Before 1.0 a derivation-version bump rebuilds the store and so empties
the table. After 1.0 nothing would.

A death serves four roles. Each needs it for a different span:

- **The death feed.** The change feed is current document rows and deaths in write
  generation order ([ADR 0047](0047-a-write-re-decides-only-the-links-whose-inputs-it-moved.md)).
  Lane 2 reads the death half for the semantic engine's retractions, for its census, and for
  the host's search margin, which counts the deaths past the engine's watermark. A death is
  needed there only until the vault's engine and its census have read past it.
- **Move-following evidence.** Repair follows an out-of-band move only from a broken link,
  and reads deaths by that link's keys. It can use only a death that a currently broken link
  names. An older death under the same name hurts it: two matching deaths make repair skip
  the link as `several_tombstones`.
- **The ambiguity class a deletion changes.** A deletion re-judges the links of the class it
  changed inside its own transaction (ADR 0047). Nothing reads the death for this afterwards.
- **Provenance under verification.** The store's verification checks each death's
  provenance against the closed vocabulary and checks that no path holds both a document
  row and a death. Both checks run over whatever rows exist, so neither needs a death kept.

The store's documentation also gave a death an ordering role: a late watcher event would
compare itself against the recorded death. No production reader does this, and none ever
did. The watcher consults the filesystem and the live document row. That role is withdrawn
from the documentation.

## Decision

### The retention rule

A death stands while either of these holds, and is pruned once neither does:

1. **Lane 2 has not read it.** The vault has a consumer position (see
   [The consumer position](#the-consumer-position)), and the death's generation is above it.
2. **A broken link names its path.** Some link holds a current `link/broken` finding and is
   held under one of the keys that `keys_naming` yields for the death's path in the
   comparison key space: the death's path key, or a segment-aligned prefix of its suffix key.
   These are the keys a link that could name the path is held under, and the comparison key
   space is the one move-following reads deaths by on every root.

A link to the same name that is healthy, because a live document now answers it, keeps no
death. An ambiguous link keeps no death either: move-following starts only from a broken
link. Quarantine deaths, whose files still stand on disk with no facts, follow the same rule.
Move-following already excludes them.

The rule is checked when the prune reaches a death, under the store's writer. A death the
rule keeps stays in place, and a later pass checks it again.

### The death floor

The store keeps the death floor as a pinned scalar in store meta. Its value is the highest
generation of any death the store has pruned. A store that has pruned nothing holds no
floor. The floor only rises: a prune writes the higher of the recorded floor and the highest
generation it pruned. It writes that value in the same transaction as the delete, so any
snapshot reads a floor that agrees with the deaths it holds. A rebuild from zero creates a
store with a new epoch and no floor.

A death-feed consumer has **read through the floor** when it has read every death recorded at
a generation at or below the floor, or a reconcile has accounted for it. A consumer shows
this in one of three ways, all within the current store epoch:

- its death-feed cursor stands at a generation above the floor;
- its last completed drain of the death feed recorded a watermark at or above the floor; or
- it reconciled against current state and recorded, beside the epoch it observed, the floor
  of the snapshot it reconciled against, and that recorded floor is at or above the store's
  floor.

A consumer that cannot show this does not seek forward, because deaths it never read are
gone. It reconciles against current state instead, exactly as it does after a store epoch
change. When the reconcile finishes, the consumer records the floor it reconciled against and
reads the feed again from the start. A reconcile stopped part way leaves the earlier floor
recorded, so the next unit resumes it, as an epoch reconcile resumes. Without the recorded
floor, a consumer that restarts from the start of the feed would stand below the floor again
and would reconcile without end.

What each consumer does below the floor:

- **The semantic engine** runs its held-path reconcile: it removes held rows whose paths the
  store no longer holds as documents, a page of held paths at a time, and then rescans both
  feeds from the start. Content-addressed sidecar rows keep the rescan to the rows that
  changed.
- **The census** resets, as it does over a new store epoch. It empties, recounts from the
  start of both feeds, and reports its last whole counts until the recount is whole.
- **The search margin** treats a death watermark below the floor as unreadable and reports
  its cap, as it does for a watermark from another store epoch. The host reads the floor from
  the same snapshot it counts the feed rows on.

The floor is part of the lane-2 read surface. The feed-read handle reports it beside the
store epoch, so the engine learns it without naming a wider store surface. Boundary
invariant 12 stays intact.

### The consumer position

The prune takes one input from outside lane 1: the **consumer position**. This is a write
generation at or below which both the vault's semantic engine and its census have read every
death. The host supplies it as the lower of the two. It derives each one only from what that
reader has itself recorded, and never above it: for a cursor, the generation before the
cursor's own; for a completed drain, its watermark. A reader that has not yet read the
current store epoch has read no death in it, and a reader that has restarted from the start
of the feed after a reconcile has read only what its new cursor shows, so the position never
runs ahead of either reader.

When the vault has no running engine, because it is disabled, detached from the vault, or not
yet started, there is no position and condition 1 of the retention rule keeps nothing.
An engine that later starts with a sidecar's old cursors meets the floor and reconciles once.

`norn-store` never depends on `norn-semantic`. The store accepts the position as a plain
generation for one prune unit. It records no consumer and registers no cursor. The host
connects the two lanes here, as it already does for the search margin, so the dependency
direction stays lane 1 ← host → lane 2.

### When the prune runs

The prune is a claim-bound maintenance unit. It deletes from the lane-1 store, so it runs
under the vault's claim in a maintenance leg on the shared worker pool, beside the shadow
sweep and the rolling-heal slice
([ADR 0051](0051-a-schema-reload-serves-reads-again-when-its-activation-ends.md)). It has its
own deadline clock with a period of 10 minutes, the shadow sweep's period. The background
scheduler keeps that clock with the other maintenance deadlines.

- **Due.** A pass comes due when its clock runs out and the store holds at least one death.
  It also stays due while a pass is unfinished. A store with no deaths owes no pass.
- **Unit.** One unit is one page of deaths in the death feed's order, `(generation, path)`.
  The page is bounded by a fixed row count. The unit checks each death on the page against
  the retention rule, deletes the deaths the rule does not keep, and raises the floor, all in
  one transaction on the store's writer. A pass ends when it reaches the end of the deaths
  that condition 1 does not keep: the deaths at or below the position, or every death when
  there is no position.
- **Bounds.** A leg checks the background turn budget between prune units, as it does
  between all its units, and always runs at least one unit. An unfinished pass keeps the
  prune due, so the leg that ends with it unfinished schedules another, and the pass resumes
  there. A leg runs its units in this order: the sweep, the prune units, then the
  rolling-heal slice.
- **No quiet wait.** The prune does not wait for a quiet entry, for the same reason the
  shadow sweep does not: a vault under continuous edits is never quiet, and that is the vault
  that accumulates deaths.

Between passes, a death that is already prunable can stand for up to about 10 minutes. If a
link naming its path breaks in that window, condition 2 keeps the death from then on. Where a
newer death answers to the same name, as when the document that answered the link has since
moved, repair skips that link as `several_tombstones` with its usual note, and the skip stands
while the link stays broken. The window in which this can arise is accepted as a brief one.

The retention check costs one indexed seek of the link keys per key the death's path yields,
plus the finding lookup for each link a seek reaches.

## Considered options

- **Keep every death.** This is today's behavior. Cost grows with the vault's deletion
  history rather than its contents, the verification passes human scale between 100,000 and
  1,000,000 deleted paths, and old deaths actively degrade move-following through
  `several_tombstones`.
- **An age horizon.** This needs a number to defend. It drops evidence repair could still
  use, from a link that broke late, and it keeps deaths nothing needs until they age out.
- **Retention pinned by any link that could name the path, healthy or not.** Common names
  such as `index` are named by some link in nearly every vault, so every dead `index` would
  be kept forever. This keeps the deaths that hurt move-following most.
- **Prune on a grace period without the engine's position.** A lagging engine, or one held
  back by real-model embedding, would fall below the floor at every prune and pay a
  whole-vault reconcile each time.
- **Wake the prune when the engine's position advances.** This needs a new scheduler wake
  path to close a window of about 10 minutes that a reported, transient skip already covers.

## Consequences

- **The death table follows the vault's present, not its history.** At rest it holds the
  deaths lane 2 has not read and the deaths a broken link names. The verification cost
  follows the same set.
- **A late link finds nothing to follow.** A link written to a path after that path's death
  was pruned finds no death and falls back to a similarity suggestion. This is the same
  answer a build from zero gives, since that build holds no deaths.
- **A dormant engine pays once.** An engine that was off when a prune ran pays one
  whole-sidecar reconcile when it next starts. A running engine never falls below the floor
  through the prune, because the position keeps every death it has not read.
- **The feed's cursor validity gains a second condition.** ADR 0047 makes a cursor valid
  within one store epoch. A cursor is now also valid only while its consumer has read through
  the death floor, and the response to either failure is the same reconcile and rescan. The
  feed stays a query over current rows. The store registers no subscriber and keeps no change
  log, so ADR 0047's rejection of a retained log with registered subscriber cursors still
  holds: the position is an argument to one unit, and nothing is evicted that a consumer
  cannot recover from current state.
- **Maintenance legs gain a third unit kind.** ADR 0051 names the shadow sweep and the
  rolling-heal slice as a leg's units and lists their deadlines. The prune pass is a third,
  and its clock is a third maintenance deadline. Every other rule of a maintenance leg
  applies to it unchanged.
- **Repair answers can depend on when the prune ran.** A link that breaks after its path's
  death was recorded follows the move if the death still stands and suggests by similarity if
  it does not, so the engine's progress and the prune's clock decide how long that evidence
  lasts. The position is a reading of how far a reader has read, not inferred state, and it
  decides only whether a death stands, never what a death or a finding says, so inference
  still never reaches findings or plans.
- **The store's documentation changes with this decision.** A death no longer stands
  exactly while its path is dead. It stands only while its path is dead, and only while this
  rule keeps it. Disjointness from document rows is unchanged.
