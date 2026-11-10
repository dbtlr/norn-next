---
status: accepted
date: 2026-11-09
---

# 0052 — A death is kept only while lane 2 or a broken link needs it

Supersedes [ADR 0047](0047-a-write-re-decides-only-the-links-whose-inputs-it-moved.md) and
[ADR 0051](0051-a-schema-reload-serves-reads-again-when-its-activation-ends.md) and restates
both whole. ADR 0047 restated ADR 0044 whole and narrowed which links a write re-decides.
ADR 0051 restated ADR 0050 whole and amended its schema reload rule, and ADR 0050, ADR 0049
and ADR 0048 before it each restated their predecessor whole and amended some of its
sections, as ADR 0051's own opening listed. The two decisions ruled two parts of the system
that a death crosses: ADR 0047 ruled the derivation lanes, link health and the change feed,
and ADR 0051 ruled reads, from-zero builds and background work. This decision adds the section
[A death is kept only while lane 2 or a broken link needs it](#a-death-is-kept-only-while-lane-2-or-a-broken-link-needs-it),
amends six sections, and copies every other section as ADR 0047 or ADR 0051 wrote it. The
sections from [The two lanes](#the-two-lanes) to the *Consequences* that follows their
*Alternatives* are ADR 0047's. The sections from the *Context* that follows them to
[Retained count results](#retained-count-results) are ADR 0051's. The amended sections are
these:

- **ADR 0047's [Lane 2 reads change through a feed](#lane-2-reads-change-through-a-feed).** A
  cursor was valid within one store epoch. It is now also valid only while its consumer has
  read through the death floor. The rejected retained change log is restated: a consumer
  pins the retention of deaths only up to a bound the vault sets, and the floor turns what
  lies past that bound into a reconcile rather than lost data.
- **ADR 0051's [Semantic work leaves the claim](#semantic-work-leaves-the-claim).** The
  drain reconciles when the engine has not read through the floor, as it does when the store
  epoch differs. Every reconcile records the write generation its first page read, never
  starts over because of the floor, and on completing records that generation itself as
  read through.
- **ADR 0051's [Work starts at a wake or a deadline, with no poll backstop](#work-starts-at-a-wake-or-a-deadline-with-no-poll-backstop).**
  The death prune's clock is a third claim-bound deadline.
- **ADR 0051's [Claim-bound slices stay maintenance legs on the worker pool](#claim-bound-slices-stay-maintenance-legs-on-the-worker-pool).**
  A death-prune unit is a third unit kind of a maintenance leg. The leg's order and the claim
  about its longest unit are restated, and a rule is inserted beside the shadow sweep's: the
  death prune does not wait for a quiet entry either, and runs only where the store's link
  health is admitted. The known limit on what a settling read waits behind counts a
  death-prune unit as a unit that may be in flight.
- **ADR 0051's [Status reports embedding coverage](#status-reports-embedding-coverage).** A
  census that has not read through the floor counts again and keeps its last whole counts
  meanwhile, which a new store epoch still clears, and it never starts that count over
  because of the floor, as a reconcile never does. A count that reaches the end of both
  feeds records the write generation it read before that final page, as a completed drain
  records its watermark.
- **The *Consequences* of ADR 0051's
  [Background work runs on its own lane](#background-work-runs-on-its-own-lane).** The costs it
  accepts name the death-prune unit beside the rolling-heal slice, and the census's whole
  reads include the recount below the floor.

ADR 0044 had every changeset re-decide every link that reaches a path it writes, "whether or
not that class's membership moved", so a write that fixed a typo in `index.md` re-decided
every `[[index]]` link in the vault, inside the write's transaction on the store's one writer
connection. **A changeset re-decides a link in another document only when it moved an input
that link's verdict reads.** A link's verdict has two inputs from outside the document
holding it: the **names** its keys reach, and the **anchor values** of the one document it
names. A write that changes neither re-decides only the links its own written documents hold.
Nothing else changes: the findings every changeset leaves standing are exactly the findings
ADR 0044's set leaves over the same facts, finding order included, apart from the write
generation each was filed at, so the narrowing is a reduction in work and never in what is
found. Link health stays on the changeset; moving it off was weighed and rejected below. A
**from-zero build** — a vault's first build and every rebuild from zero, the construction
[ADR 0045](0045-a-from-zero-build-writes-once-in-steps.md) builds in steps — still judges link
health once, after every fact has landed, as ADR 0044 ruled and this decision restates.

Background work is split by whether it needs the vault's claim. Work that needs no claim
gets its own threads in `norn-host`, the **background lane**: an engine's initialization
and feed consumption, and the store's whole periodic verification, whose one write runs
on a copy of the store. Neither holds a vault claim, so neither delays that vault's
writes, and long semantic work never takes a slot of the shared worker pool. Work that
needs the claim stays in maintenance legs on the shared worker pool, as today: the
rolling-heal slice and the shadow sweep. A
maintenance leg now also ends at the background turn budget. Both kinds of work start when
a wake or a deadline says they are owed, and no poll tick starts either. The section
[Background work runs on its own lane](#background-work-runs-on-its-own-lane) records the
change.

A **from-zero build** — a vault's first build and every rebuild from zero — is the
construction over which link health is judged once, as
[A from-zero build judges link health once](#a-from-zero-build-judges-link-health-once)
rules, under the same term. Whole-vault readiness retains its
existing meaning.

## A death is kept only while lane 2 or a broken link needs it

A **death** is the record a document's removal leaves in the store: the path, the content
hash it was last derived at, the provenance of the news and the write generation it was
recorded at. The store holds it as a row of its `tombstones` table, and **tombstone** names
the same record. Until this decision a death was removed only when its path derived again,
so the table grew with every distinct path the vault ever deleted. A death is now kept
while lane 2 has not read it, up to a bound the vault's size sets, and while move-following
would read it for a link that is broken now. A claim-bound maintenance unit, the **death
prune**, removes every other death. The name sets it apart from a heal's prune, which removes
the documents a walk no longer finds and records their deaths. The store records the highest
generation of any death the death prune has removed as the **death floor**, and a
death-feed consumer that has not read through the floor reconciles against current state
instead of seeking forward, as it does after a store epoch change.

### Context

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
  generation order ([Lane 2 reads change through a feed](#lane-2-reads-change-through-a-feed)).
  Lane 2 reads the death half for the semantic engine's retractions, for its census, and for
  the host's search margin, which counts the deaths past the engine's watermark. A death is
  needed there only until the vault's engine and its census have read past it.
- **Move-following evidence.** Repair follows an out-of-band move only from a broken link.
  The move-following reads return at most two deaths for that link, and repair decides only
  between one and more than one: one death is followed, and two make repair skip the link as
  `several_tombstones`. The only deaths that can change repair's answer are the at most two
  those reads return for a link that is broken now.
- **The ambiguity class a deletion changes.** A deletion re-judges the links of the class it
  changed inside its own transaction, deriving the class from the path it deletes
  ([Link health is re-decided inside the changeset](#link-health-is-re-decided-inside-the-changeset)).
  Nothing reads the death for this afterwards.
- **Provenance under verification.** The store's verification checks each death's
  provenance against the closed vocabulary and checks that no path holds both a document
  row and a death. Both checks run over whatever rows exist, so neither needs a death kept.

The store's documentation also gave a death an ordering role: a late watcher event would
compare itself against the recorded death. No production reader does this, and none ever
did. The watcher consults the filesystem and the live document row. That role is withdrawn
from the documentation.

### The retention rule

A death stands while either of these holds, and the death prune removes it once neither
does:

1. **Lane 2 has not read it, within the vault's bound.** The vault has a read-through
   generation (see [The read-through generation](#the-read-through-generation)), the death's
   generation is above it, and the death is one of the newest deaths the bound admits. The
   bound is the vault's live-document count, the number of document rows the store holds: no
   more unread deaths are kept by this condition than that. Every unread death counts toward
   the bound, one a broken link also keeps and a quarantine death included. Past the bound,
   the oldest unread deaths in feed order are not kept by it.
2. **Move-following would read it for a broken link.** Some link holds a current
   `link/broken` finding under the pinned vault schema fingerprint, and the death is one of
   the at most two deaths the move-following reads return for that link, in the order they
   return them. A suffix-addressed link reads `tombstones_by_suffix` over its target. A
   path-addressed link reads `tombstone_at_path` at each path its address names from its
   holder, and the deaths are pooled distinct by path, in path order, stopping at two.

A healthy link keeps no death, and neither does an ambiguous one: move-following starts only
from a broken link. A broken link keeps at most two deaths, however many share its name: a
third death in its read's order is not one repair can reach. A quarantine death is never kept
by a link, because move-following excludes quarantine deaths, so it stands under condition 1
alone.

**Repair and the death prune reach a link's deaths through one routine.** The address split
above is the one repair runs today, and the deaths a link keeps must be the deaths repair
reads for it by construction rather than by agreement between two copies. The routine that
turns a stored link into its move-following reads therefore lives once, in `norn-store`
beside the reads themselves, and both repair and the death prune call it. Its inputs, the
stored link's family, protocol and target and its holder's path, are all lane-1 facts the
store holds.

**Move-following answers the same before and after a death prune.** For every link broken
when a death-prune unit commits, each move-following read returns the same deaths after the
unit as before it. The unit removes no death a broken link keeps, and removing any other
death cannot change what a read returns: the reads walk indexes that run on to `path`, so the
deaths that remain keep the order they held, and the two a read returned are still first. A
test pins this property: over broken links whose names collide with older and newer deaths,
by suffix and by exact path, every move-following read for every broken link is compared
before and after each unit.

**The rule bounds the table.** At rest the table holds at most two deaths per broken link
and at most one unread death per live document. Its size follows the vault's present, its
documents and its broken links, never its history of deletions.

**The bound is the live-document count because past it a reconcile costs about what the
drain it replaces costs.** A reader that falls past the bound reconciles once, when it next
advances. The reconcile checks each path the reader holds once against the store's document
rows, and then rescans the document feed, re-embedding nothing whose content the sidecar
already holds. A stalled sidecar still holds a path for each death it has not read, so it
can hold more paths than the vault now has documents, and its checks of those paths cost
what retracting those deaths through the feed would. What the reconcile adds to the drain
is the rescan, whose cost follows the vault's live documents. Past the bound the unread
deaths outnumber the live documents, so the reconcile costs within a small factor of
draining them, and keeping those deaths would charge every pass, every verification and the
store's size for the deaths only a stalled reader would read.

### The death floor

The store keeps the death floor as a pinned scalar in store meta. Its value is the highest
generation of any death the death prune has removed. A store whose death prune has removed
nothing holds no floor. The floor only rises: a death-prune unit writes the higher of the
recorded floor and the highest generation it removed. It writes that value in the same
transaction as the delete, so any snapshot reads a floor that agrees with the deaths it
holds. A rebuild from zero creates a store with a new epoch and no floor.

A death-feed consumer has **read through the floor** when its read-through generation, as
the next section defines it for each reader, is at or above the floor. A consumer that has
not read through the floor does not seek forward, because deaths it never read may be gone.
It reconciles against current state instead, exactly as it does after a store epoch change,
and then reads the feed again from the start.

**A reconcile accounts for every death committed before it began.** On its first page a
reconcile reads the store's write generation, here called G0, and records it beside the
store epoch and cursor the reconcile already records. The write generation is the last
committed generation, so every death at or below G0 was committed before that page, and the
reconcile meets its path either as no document or as a document derived again, whether or
not the death still stands: a reconcile against current state accounts for it. When the
reconcile completes, the consumer records G0 itself beside the epoch it observed, as the
generation it has read through. A reconcile never starts over because of the floor. Where
the floor stands at or below G0 when it completes, the consumer has read through it, and the
rescan after the reconcile does not reconcile again. Where the death prune raised the floor
past G0 while the reconcile ran, the consumer is still below the floor when it completes,
and it reconciles again, from a new first page, when it next advances. A reconcile stopped
part way leaves the earlier records, so the next unit resumes it with its G0, as an epoch
reconcile resumes. A reconcile over a new store epoch records G0 in the same way: a new
store's death prune can run before the epoch reconcile completes, and without the record the
consumer would reconcile a second time.

**A reconcile completes, and a quiet store needs exactly one.** One changeset stamps every
death it records with one generation, and a death-prune unit takes no generation of its own,
so the death prune can raise the floor to exactly the write generation a reconcile's first
page reads. A changeset that deletes 4,000 of 6,000 documents leaves 2,000 live documents,
and the cutoff lands inside that changeset's generation; with no live documents left, any
single deletion does the same. A reconcile that started over wherever a page read a floor at
or above G0, or that recorded G0 − 1, would then never leave the floor in a quiet store:
every first page would read that G0, every later page that floor, and a reconcile of one page
would record a generation still below it. Recording G0 claims nothing the reconcile did not
account for, so a floor at G0 is one the consumer has read through. The floor passes G0 only
where the death prune removed a death recorded after the first page, and it removes such a
death, which the reconciling reader has not read, only past the bound, oldest first in feed
order, so only where more deaths than the vault's live-document count were recorded while
the reconcile ran. The consumer then reconciles again. No reconcile is abandoned part way, and each one
that completes removes the rows it found stale, so repeated reconciles converge under any
rate of deletion. A held path checked live on an early page, then deleted and its death
removed before the reconcile completes, stays accounted for: that death's generation is
above G0, so the floor ends above the recorded G0, and the next reconcile removes the path's
rows.

Recording G0 rather than the floor is what lets a lagging reader leave the floor behind.
Had the reconcile recorded the floor it read, an engine whose backlog stands past the bound
would sit at that floor, the next eviction would raise the floor past it, and it would
reconcile again at every pass, while its census, which counts one page of each feed per
unit, never finished a count. G0 lies at or above the whole backlog the reconcile accounted
for, so the next eviction stays at or below it.

What each consumer does below the floor:

- **The semantic engine** runs its held-path reconcile. A page of held paths at a time, it
  checks each path it holds against the store's document rows and removes the rows whose
  paths the store no longer holds as documents. It then rescans both feeds from the start.
  Content-addressed sidecar rows keep the rescan from re-embedding anything whose content the
  sidecar already holds. The reconcile therefore costs one check per held path and a read of
  the document feed and of the deaths the store keeps. A stalled sidecar holds a path for
  each death it has not read, so it can hold more paths than the vault now has documents.
- **The census** counts again. It empties its rows and reads both feeds from the start,
  recording G0 on its first page and G0 itself as read through on completing, as a
  reconcile does, and never starting the count over because of the floor. A floor reset is
  not an epoch reset: the database is the same, so the census keeps its last whole counts,
  and status answers them until the new count is whole. A recount that completes while the
  floor stands above its G0 counts again, as a reconcile reconciles again, and keeps those
  last whole counts across it and nothing of the count before: it empties the rows it has
  counted and reads a new G0 on its new first page. A new store epoch still clears the last
  whole counts and restarts the count, because no count is carried from one database to
  another.
- **Freshness and the search margin** judge the engine's own read-through generation against
  the floor, never its death watermark alone. A watermark is set only when a drain of the
  death feed completes, so an engine part way through a long backlog, whose death cursor has
  read past the floor, holds a watermark below it although the death prune removed only
  deaths it had read. Where the engine's read-through generation in the reading's store
  epoch stands below the reading's floor, freshness reports the engine as rescanning and the
  unfiltered vector rung's margin is its cap, as they do for a watermark from another store
  epoch. Otherwise
  each reads its lag from the watermarks as before. Every death the prune removed stands at
  or below the floor, which the engine has then read through, so the feed rows standing past
  the watermarks still bound what the engine has not read.

**The readings the host judges carry what the judgment reads.** The engine's report, and the
sample of the engine a search takes under its slot lock, carry the engine's read-through
generation and the store epoch it was recorded in, beside the watermarks. The store reading
each judges against carries the death floor beside the store epoch and the write generation,
read in the same snapshot, whether it is the reading of a search's own hold or the reading a
vault's entry recorded.

**A floor raise reaches the host as a lane-1 commit does.** A death-prune unit that raises
the floor records the store reading the host's status judges against, as a leg records it
where its lane-1 work commits. Where the new floor stands above the read-through generation
the unit was handed, the raise removed deaths a reader had not read, so the unit is a commit
that leaves the engine owing work and sends that wake. A status taken after the raise
therefore judges against the new floor, and where the engine has not read through it,
reports the engine rescanning until it reconciles. It never reports `trailing(0)` against an
older reading while the sidecar holds rows for deaths the raise removed unread. A unit that
raises no floor records nothing and sends no wake.

**A death-prune unit takes no write generation.** The write generation orders the writes a
read can observe, and every reading, cursor and watermark is a position in that order. A
death-prune unit changes nothing a lane-1 read answers: move-following answers the same
before and after it, and no other read reads a death. Its one effect a reader can see is on
the death feed, and the floor states that effect. A generation would make every reading
taken across the unit report a write that changed no answer, and freshness report a lag that
no drain has work for. A unit therefore moves the store reading's floor and never its write
generation, and a pass's upper bound is always the generation of a write.

The floor is part of the lane-2 read surface. The feed-read handle reports the floor read in
each page's snapshot, beside the store epoch its source was minted for, which the source
checks once, when its connection opens. The engine learns the floor without naming a wider
store surface, and boundary invariant 12 stays intact.

### The read-through generation

The death prune takes one input from outside lane 1: the **read-through generation**. It is
a write generation at or below which both the vault's semantic engine and its census have
read every death in the current store epoch, or accounted for it by a reconcile. The host
supplies it to each death-prune unit.

**Each reader's read-through generation comes only from what that reader has recorded in the
current store epoch, and never runs above it.** It is the highest of:

- the generation before its death cursor's own;
- the watermark of its last completed drain of the death feed, for the engine, or the
  completion record of its last count that reached the end of both feeds, for the census;
  and
- the generation its last completed reconcile recorded, the G0 that reconcile's first page
  read.

A reader that has recorded none of these in the current store epoch has read no death in it.
While a reconcile or a census recount runs, the reader's death cursor counts toward none of
these: its read-through generation stays what its records said when the reconcile or recount
began, until it completes; the reader stores that value with the reconcile or recount, so a
restart resumes with it (a value lost to a restart only falls, which is safe). A recount reads the death feed again from the start, and a death
its cursor passes mid-recount is not yet one it has accounted for, since a death recorded
after the recount's first page may have been removed before the cursor reached it.

**The census records a completion, as the engine records a watermark.** When a count reaches
the end of both feeds, the census records, beside the store epoch it observed, the write
generation it read before that final page. Every death at or below that generation was
committed before the page. Outside a recount, the census checks, before each page it reads,
the floor read in that page's own snapshot against the read-through generation it held before
the page, so where the final page's floor stands at or below its read-through generation no
death it had not read was removed, and it records the completion. A recount records it only
where the final page's floor stands at or below the recount's G0: no death above G0 can have
been removed without the floor passing G0, because the census's read-through stays below G0
for the whole recount and the death prune removes an unread death only past the bound, which
raises the floor to it. A recount whose final page reads a floor above its G0 records its G0
alone and counts again, the one rule a reconcile follows too. Without the
record, the census's read-through generation would rest on its death cursor and stop one
below the newest death's generation, so the deaths of the newest generation would stand
until some later death was recorded.
The host supplies the lower of the two readers' read-through generations, and where either
reader has read nothing, the lower is nothing: condition 1 then keeps every death, up to the
bound.

**Only an engine turned off in the vault's configuration gives no read-through generation.**
Where the vault's engine section is absent or states the engine off, no reader of the death
feed stands for the vault, and condition 1 keeps nothing. A host that composes no engine at
all stands no reader for any vault, and is treated the same way. Every other vault's engine
counts as a reader, whether it is running, preparing, detached from its slot for a moment, or
self-disabled, and so does an engine whose section could not be read. Such an engine is
handed as having read nothing, since the host reads the report its slot's last hold left and
that report is not the engine's sidecar. This is conservative: a pass handed less than the
engine has read removes a subset of what it could, and keeps the unread deaths, up to the
bound. An engine turned on later with a sidecar's old
cursors meets the floor and reconciles once.

`norn-store` never depends on `norn-semantic`. The store accepts the read-through generation
as a plain generation for one death-prune unit. It records no consumer and registers no
cursor. The host connects the two lanes here, as it already does for the search margin, so
the dependency direction stays lane 1 ← host → lane 2. The engine's report carries each
reader's read-through generation as it carries the watermarks, and the host reads it from the
report, never from the sidecar.

### When a death prune runs

The death prune deletes from the lane-1 store, so it runs under the vault's claim, in a
maintenance leg on the shared worker pool, beside the shadow sweep and the rolling-heal slice
([Claim-bound slices stay maintenance legs on the worker pool](#claim-bound-slices-stay-maintenance-legs-on-the-worker-pool)).

- **Where.** A death-prune unit runs only over a store whose from-zero build has ended and
  whose link health is admitted, so never during a from-zero build or while link health is
  loading, and only in a maintenance leg, which never runs while the entry owes a heal rung.
  It does not wait for the entry to publish `Ready` again after each change, nor for a quiet
  entry, for the reason the shadow sweep does not: a vault under continuous edits is never
  quiet, and that is the vault that accumulates deaths. Its findings are exact between
  changesets, which is all it reads them at.
- **Clock.** The death prune has its own deadline clock with a period of 10 minutes, the
  shadow sweep's period. The clock starts one period after attach. When it comes due, a pass
  is due only where new work could exist:
  - a death has been recorded above the last pass's bound;
  - a `link/broken` finding has been filed or discarded since the last pass began, which the
    host reads off the store's marks at a due check: the newest death, the newest broken
    finding and the count of broken findings under the pinned fingerprint. Its cost follows
    the vault's broken findings, and it runs only at a due check; or
  - the last pass kept a death for lane 2 alone, and the read-through generation the host
    supplies has risen above the lowest one any of that pass's removing units was handed,
    so lane 2 may have read it.

  A death past the bound of condition 1 can arise only from a newly recorded death, so the
  first case covers the bound. The third case compares against the lowest generation the
  pass's removing units were handed because every death a unit kept for lane 2 stands above
  the generation that unit was handed, so lane 2 can have read none of them until the
  read-through generation rises above the lowest of those. A configured engine that never
  advances leaves its read-through generation where it stood, so it makes no pass due by the
  third case, and a stalled engine does not have every broken link marked again each period.
  An engine turned off after a pass makes the third case hold, since the host then hands no
  read-through generation and a pass keeps nothing for lane 2. The third case has no wake of
  its own: a rise in the read-through generation is picked up at the next deadline the
  scheduler holds, at most about one period later.
  A pass in progress keeps the death prune due. A store where none of these holds owes no
  pass, and its clock contributes no deadline until one does. The attachment holds what the
  cases read, among them the lowest read-through generation any removing unit of the last
  completed pass was handed, so a restart or a new attach owes one pass at its first period.
- **A pass.** A pass fixes its upper bound, the store's write generation, at its first unit.
  Deaths recorded after it are left to a later pass. It runs in three phases, each resuming
  from an exclusive cursor the attachment holds:
  1. **Marking.** It reads the `link/broken` findings under the pinned fingerprint in the
     order they were filed, `(generation, path, position)`, where a finding's position is its
     link's ordinal in the holding document, runs the move-following reads for
     each one's link, and records the deaths they return in a **kept set**: a table in the
     temporary database of the store's writer connection, as the rolling heal keeps its
     inventory.
  2. **The bound.** It counts the live documents, L. It then walks the deaths at or below the
     pass's upper bound in descending feed order, `(generation, path)`, starting from the
     newest of them. It counts every death it passes, one a broken link keeps and a
     quarantine death included. It stops once it has passed L deaths, or once it reaches a
     death at or below the read-through generation or the start of the feed, whichever comes
     first. Where it stops short of L deaths, condition 1 keeps every unread death and the
     pass has no cutoff. Where it passes L deaths first, the next death it meets, the
     (L + 1)th newest at or below the upper bound, is the **cutoff**, if it stands above the
     read-through generation: the cutoff and every unread death before it in feed order are
     past the bound. The cutoff is fixed for the rest of the pass, and no removing unit
     computes it again.
     A pass whose marking ends while its unit is handed no read-through generation skips the
     count and the walk, since condition 1 keeps nothing; should a later unit of that pass be
     handed one, the pass has no cutoff and keeps every death above it.
  3. **Removing.** It reads the deaths in feed order, `(generation, path)`, up to the pass's
     bound, and deletes each death that the kept set does not hold and condition 1 does not
     keep, raising the floor in the same transaction. Condition 1 keeps a death above the
     read-through generation the unit was handed and after the cutoff in feed order.

  **The bound walk is pinned so that the table stays bound by the vault's present and the
  removal is deterministic.** The walk starts at the newest death at or below the pass's
  upper bound rather than the newest in the store, so the cutoff follows the deaths the pass
  covers and not how far writes got before the walk ran; deaths recorded after the upper
  bound are left to the next pass, which counts them. Every death counts, whatever else keeps
  it, so condition 1 keeps at most L deaths, and the table at rest holds at most L deaths for
  lane 2 and two for each broken link; each step of the walk is then one index step, with no
  lookup into the kept set. The cutoff is fixed for the pass because computing it again would
  repeat the walk's L steps in every removing unit, and a cutoff that moved between units
  would make which deaths a pass removes depend on when its units ran. The deaths before the
  cutoff are the oldest unread deaths, so the first death the bound evicts is the oldest
  unread death no broken link keeps, and the newest it can evict is the cutoff itself. A death
  the walk counted that a changeset then clears, because its path derived again, leaves fewer
  than L deaths kept for lane 2 until the next pass, never more.

  **Every phase continues by an exclusive keyset seek.** Marking seeks the partial findings
  index below past the last `(generation, path, position)` it marked under the pinned
  fingerprint. The bound walk and the removal seek `tombstones_change_feed` past the last
  `(generation, path)` they read, the walk descending and the removal ascending. No page
  steps over rows an earlier page consumed, and none sorts. Each statement carries plan bars,
  as the change-feed drains do: a search of its index bounded by its cursor, no scan of the
  table, and no temporary b-tree for its order.

  **Every removing unit first marks the broken findings filed since the marking cursor.** The
  kept set it deletes against therefore covers every link broken in that unit's own
  transaction, which is what the property above needs. A changeset that breaks a link,
  refiles its finding, or moves the deaths a broken link's read returns files that link's
  finding again at a newer generation, so the catch-up reaches it. A finding discarded since
  it was marked leaves deaths in the kept set that no link needs; they stand until the next
  pass, which is conservative. No changeset commits between two units of one leg, so the
  catch-up costs nothing after a leg's first unit, and across legs it costs the broken
  findings the writes between them filed.

  **The marking order needs an index the store does not hold.** `findings_fingerprint_kind_nocase`
  reaches every `link/broken` finding under a fingerprint, but in path order, so it cannot
  find the findings filed since a cursor. `finding_classes_class_key`,
  `finding_paths_path_key` and `findings_one_per_link` reach findings by key or by link, not
  by kind. The death prune adds one partial index over `link/broken` findings, keyed by
  `(vault_schema_fingerprint, generation, path, position)`. A changeset writes one entry in it
  for each broken finding it files and removes one for each it discards, so its write cost
  follows the broken findings a changeset touches, never the vault. A bulk re-judgment, such
  as the re-decision of every link a schema pin owes, pays one entry for each broken finding
  it files or discards. The implementation measures that cost on a bulk re-judgment and
  records it beside the index in `ddl/findings.rs`.

  A pass that finds a pinned fingerprint other than the one it began under starts over,
  because the findings it marked from are gone, and it clears its kept set first. A pass that
  ends clears its kept set too. A host restart, a detach or a rebuild loses the pass's
  cursors and its kept set, and the next pass starts over.
- **Units.** A death-prune unit is one transaction on the store's writer. It is bounded by the
  logical rows it reads, findings, deaths, kept-set rows and index steps together, and not
  only by the rows it deletes, so a page of kept deaths cannot hold the writer longer than a
  page of deleted ones. The kept-set rows a unit clears, at a pass's end or at a fingerprint
  restart, count toward the same bound, so clearing a large kept set takes several units. A
  removing unit is also bounded by the deaths it deletes, because a deletion writes a page of
  each path-ordered index over the deaths and costs far more than a read. The implementation
  sizes both bounds by measurement so that one unit's own work costs no more than a
  rolling-heal slice, about 13 ms at 10,000 documents, and records the measurement beside
  the values. A write-ahead-log checkpoint is not part of that cost: SQLite runs one inside whichever
  writer's commit takes the log past its threshold, a watcher write, a plan apply or a
  rolling-heal slice as much as a death-prune unit, and a pass that removes many deaths
  crosses it more often than an ordinary writer does. That cost, and the sync stalls it can
  meet on a large store, belong to the store's checkpoint policy, which this decision does
  not change. A unit
  whose catch-up fills its bound deletes nothing, and the next unit continues the catch-up.

  **The live-document count is the one read a row bound cannot cut.** It is one statement,
  run once per pass at the start of the bound phase, and it counts the document rows whole,
  stepping every entry of an index over `documents`, so its cost follows the vault's live
  documents. The unit that runs it runs nothing else. The implementation measures the count
  at the largest vault it supports and records that one count costs no more than a unit is
  allowed; a count that does not fit is a defect to diagnose, not a bound to raise.
- **Leg order.** A leg runs the sweep, then the rolling-heal slice, then death-prune units.
  The slice runs at most once in a leg and comes before the death prune, so the death prune
  cannot starve it. A leg checks the background turn budget between units, as it does between
  all its units, and always runs at least one unit. A leg that ends with a pass unfinished
  leaves maintenance due, so another leg follows, as a rolling-heal pass does.

**A pass costs in proportion to what it keeps and examines.** Its work is one move-following
read per broken link, one more per broken finding filed while it runs, one count of the live
documents and a walk of at most L + 1 deaths for the bound, one step per death up to its
bound, and the clearing of its kept set. The deaths up to its bound are the ones it
kept last time, at most two per broken link and one unread death per live document, plus the
deaths recorded since. A healthy link costs a pass nothing, and neither does the vault's
history of deletions.

### How long a death waits

A death that lane 2 has not read stands until lane 2 reads it or until the bound passes it.
From then, a death no broken link keeps stands until the next pass reaches it: at most one
period of the death prune's clock, plus the length of that pass, because lane 2's reading
it raises the read-through generation, which leaves the next pass due. Lane 2 has read a
death of the newest generation once the engine's drain of the death feed has completed past
it and the census's count has reached the end of both feeds past it: a death cursor alone
reads through only the generation before its own, so the engine's watermark and the census's
completion record are what carry the read-through generation over the newest generation's
deaths. A death a broken link
keeps stands while that link stays broken and the death is one of the two the link's reads
return. Under sustained bulk refiling of broken findings, removal can wait longer, as the
known limits below state.

The death prune adds no window in which repair answers differently. For a link that stays
broken, the move-following reads return the same deaths whether or not a death prune ran,
so a `several_tombstones` skip lasts exactly as long as it would with every death kept: until
the link stops being broken, or one of its two deaths is cleared because its path derived
again. A link whose deaths a pass removed while it was healthy, ambiguous or held nowhere,
and which then breaks, finds fewer deaths than it would have, as the consequences below
state.

### Known limits

**A link whose holder is withdrawn for a time keeps no death while it is withdrawn.** A
holder that becomes unreadable keeps its names alone
([ADR 0046](0046-one-unreadable-path-never-breaks-the-vault.md)), a quarantined holder has no
row, and an ignored holder is not in the store, so none of them holds a stored link or a
`link/broken` finding. A pass in that time may remove a death the link would keep. When the
holder returns, its link may find that its move's death is gone. The worst outcome is that a
move cannot be followed, and repair falls back to a similarity suggestion.

**A link whose target lies beneath an unreadable-directory mark keeps no death while the
mark stands.** A path-addressed link beneath a directory marked unreadable that resolves to
no held name is not judged
([Link health is re-decided inside the changeset](#link-health-is-re-decided-inside-the-changeset)),
so its `link/broken` finding is set aside while the mark stands, and a pass in that time may
remove the deaths its reads would return. When the directory lists again and the mark ends,
the link is judged broken again with nothing to follow. The worst outcome is the same: a
move cannot be followed, and repair falls back to a similarity suggestion.

**Under sustained bulk refiling of broken findings, removal waits.** A changeset that
rewrites a document holding many broken links files all their findings again, and every
removing unit first marks the findings filed since its marking cursor. Where holders with
many broken links are rewritten again and again, faster than units mark their findings, the
catch-up can take each unit's whole allowance, and those units remove nothing. Deaths then
accumulate only while those writes continue, and the units after them drain the backlog.
Removal never skips the catch-up to make progress: a unit that deleted against a kept set
missing a link broken in its own transaction could remove a death that link's reads return,
and move-following would no longer answer the same before and after the unit.

**A pass whose read-through falls mid-pass can keep more than the bound for that pass.** The
read-through generation a unit is handed can fall within a pass, as when the engine's slot is
replaced and its report reads nothing. A removing unit handed the lower value keeps every
death above it, though the pass's cutoff was fixed against the higher one. The floor is
unaffected, since it rises only to a death a unit removed, and the next pass is owed once the
read-through generation rises again.

### Considered options

- **Keep every death.** This was the behavior before this decision. Cost grows with the
  vault's deletion history rather than its contents, the verification passes human scale
  between 100,000 and 1,000,000 deleted paths, and old deaths actively degrade
  move-following through `several_tombstones`.
- **An age horizon.** This needs a number to defend. It drops evidence repair could still
  use, from a link that broke late, and it keeps deaths nothing needs until they age out.
- **Retention pinned by any link that could name the path, healthy or not.** Common names
  such as `index` are named by some link in nearly every vault, so every dead `index` would
  be kept forever. This keeps the deaths that hurt move-following most.
- **Retention by every death a broken link's name reaches, checked from each death.** A death
  would stand while any broken link is held under one of the keys its path yields. A broken
  `[[index]]` would keep every dead `index` the vault ever had, though repair reads only two,
  and the check would seek, for each death, every link sharing its name, healthy links
  included, so its cost would follow the most common names in the vault. Working from each
  broken link to the two deaths its reads return keeps exactly what repair can use, at one
  read per broken link.
- **Unbounded retention for lane 2.** An engine that is configured but never advances, such
  as one whose section cannot be read or whose model will not load, would keep every death
  the vault records, for as long as it stays so. Past the live-document count, the reconcile
  the bound turns the backlog into costs within a small factor of draining it, and the store
  would keep, for that engine alone, deaths that every pass and verification pays for.
- **Remove deaths without the read-through generation.** A lagging engine, or one held back
  by real-model embedding, would fall below the floor at every pass and pay a whole-vault
  reconcile each time.
- **Restart a pass whenever the broken-link set changes.** Every write to a document holding
  a broken link files its findings again, so under steady edits a pass would never finish.
  The catch-up keeps each unit exact at the cost of the findings filed meanwhile.
- **Wake the death prune when lane 2 advances.** This needs a new scheduler wake path. A rise
  in the read-through generation after a pass that kept a death for lane 2 alone leaves the
  next period due instead, which bounds the wait by one period.
- **Make a pass due whenever the last one kept a death for lane 2 alone.** A configured
  engine that never advances would then make every period owe a pass, which marks every
  broken link again and removes nothing.
- **Record the floor a reconcile started against as its read-through generation.** An engine
  whose backlog stands past the bound would sit at that floor, the next eviction would pass
  it, and it would reconcile at every pass while its census never finished a count.

### Consequences

- **The death table follows the vault's present, not its history.** At rest it holds the
  deaths lane 2 has not read, at most one per live document, and at most two deaths per
  broken link. The verification cost follows the same set.
- **A late link finds less to follow.** A link written to a path after that path's death was
  removed, or one that breaks after its deaths were removed while it was healthy, finds
  fewer deaths than it would have, or none, and falls back to a similarity suggestion where
  it finds none. A build from zero holds no deaths at all, so it gives the same answer.
- **A reader past the bound pays once.** An engine that was turned off when a pass ran, or
  that fell further behind than the vault's live-document count, pays one reconcile when it
  next advances, and its census counts again. The reconcile costs one check per path the
  sidecar holds, which for a stalled sidecar can exceed the vault's documents, and a rescan
  of the document feed that re-embeds nothing whose content the sidecar holds. It accounts for
  every death committed before it began and never starts over, so it reconciles again only
  where more deaths than the vault's live-document count are recorded while it runs, and
  once complete it does not reconcile again for the backlog it accounted for. A reader within the bound never falls
  below the floor, because condition 1 keeps every death it has not read.
- **The feed's cursor validity gains a second condition.** A cursor is valid within one store
  epoch and only while its consumer has read through the death floor, and the response to
  either failure is the same reconcile and rescan. The section
  [Lane 2 reads change through a feed](#lane-2-reads-change-through-a-feed) records it.
- **Maintenance legs gain a third unit kind.** The death-prune unit joins the shadow sweep
  and the rolling-heal slice, and its clock is a third maintenance deadline. Every other rule
  of a maintenance leg applies to it unchanged.
- **Repair answers can depend on when a pass ran, but only for a link that was not broken
  throughout.** The read-through generation is a reading of how far a reader has read, not
  inferred state, and it decides only whether a death stands, never what a death or a
  finding says, so inference still never reaches findings or plans.
- **The engine's progress can lengthen how long an unread death stays, and this is
  accepted.** Under a real model the engine's pace sets how long lane 2 keeps a death, and so
  whether repair can follow a move for a link written after the death, or one that breaks
  late. Repair's answer for such a link already depends on timing, through the death prune's
  clock. The live-document bound caps the stretch the engine adds, at one unread death per
  live document. Repair never receives a wrong fix from it: it follows a move only while the
  death that records the move exists, and that death records a real move. In no case does
  the model decide what repair reads from a death.
- **The store's documentation changes with this decision.** A death no longer stands exactly
  while its path is dead. It stands only while its path is dead, and only while this rule
  keeps it. Disjointness from document rows is unchanged.
- **The churn suite certifies recording, not keeping.** Every place a churn phase removed is
  a death the store holds at that phase's settle, because the death prune's first period
  after attach is longer than any phase's window. The suite therefore certifies that each
  removal recorded its death, and the store's own suite certifies how long a death is kept.

These consequences are testable:

- For every link broken when a death-prune unit commits, each move-following read returns
  the same deaths before and after the unit.
- A broken link keeps at most the two deaths its reads return. A healthy or ambiguous link
  keeps none, and no link keeps a quarantine death.
- Condition 1 keeps every death above the read-through generation, up to the live-document
  count, and past that count the death prune removes the oldest unread deaths first and
  raises the floor. Every unread death counts toward the bound, link-kept and quarantine
  deaths included; the cutoff is the (L + 1)th newest death at or below the pass's upper
  bound, and it stays fixed for the pass.
- A vault whose engine is turned off keeps no death for lane 2. A configured engine that has
  recorded nothing in the store epoch keeps every unread death, up to the bound.
- A reconcile, an epoch reconcile and a census recount each record the write generation G0
  their first page reads, never start over because of the floor, and on completing record G0
  itself as read through, so where the floor stands at or below G0 the rescan after it does
  not reconcile again, and where the floor passed G0 while it ran the reader reconciles
  again. In a quiet store whose floor stands at exactly the current write generation, as
  after one changeset deletes most of the vault or, with no live documents, after any one
  deletion, a reconcile and a census recount each complete once, and the reader has then
  read through the floor. An engine whose
  backlog stands past the bound, with fewer deaths than the live-document count recorded
  while it reconciles, reconciles once, and its census finishes its count. A held path
  checked live on an early page, then deleted and its death removed before the reconcile
  completes, leaves the reader below the floor, and the next reconcile removes it.
- A census count that reaches the end of both feeds records the write generation it read
  before that final page, where that page's floor stands at or below what the census had
  read through (a recount: at or below its G0), and its read-through generation then covers
  the newest generation's deaths. A recount whose final page reads a floor above its G0
  records its G0 alone and counts again; while a recount runs its death cursor raises no
  read-through generation, so a recount that counts a document live and then meets its
  pruned death past its cursor still counts again.
- A census below the floor reports its last whole counts until its recount is whole, a
  second recount included. A census over a new store epoch reports none; where a floor
  already stands in that epoch, it starts as a recount.
- Freshness reports rescanning, and the search margin its cap, where the engine's
  read-through generation stands below the floor of the reading they judge against, and not
  where only its death watermark does.
- A death-prune unit that raises the floor records the store reading status judges against,
  and wakes the engine where the floor passed the read-through generation it was handed. It
  takes no write generation.
- A death-prune pass comes due only where a death was recorded, a broken finding filed or
  discarded, or the read-through generation rose above the lowest one the removing units of
  a pass that kept a death for lane 2 alone were handed, and never during a from-zero build or while link health is loading. A configured
  engine that never advances makes no pass due through the read-through generation.
- Every phase of a pass continues by an exclusive keyset seek with no sort, and the
  live-document count fits one unit.
- A death-prune unit reads no more logical rows than its bound, and a leg with a rolling-heal
  slice due runs the slice before any death-prune unit.

## The two lanes

Every unit of derived state beyond the core document projection is an **index projection**:
a declaration naming its inputs — vault-document bytes, or named lane-1 pillars — whether it
is deterministic, and the one key that invalidates it wholesale. The lane is derived from
the declaration, never chosen. A deterministic projection of source bytes is **lane 1**:
written inside the atomic changeset, scoped to the changed set, and covered by the
convergence-to-equivalence bar. The link-health finding family is lane 1 too, by the ruling
below and within its link neighborhood; no other declaration whose inputs reach across
documents is lane 1 without a ruling of its own. Everything else is **lane 2**:
asynchronous, eventually consistent, and maintained by an **engine** — a domain that owns a
sidecar database and derives from committed lane-1 records, never from vault files.

Lane 1 is closed to everything else by ruling rather than by case-by-case review: what may
ride the changeset is decided once, per family, and the bars test that what rides it keeps
lane 1's four obligations, which all assume deterministic, bounded locality:

- **atomicity** — the changeset is one transaction, scoped to the blast radius of what it
  changes, and so is every other lane-1 transaction that re-decides, as ruled below;
- **bounded working memory** — peak memory is a function of the working set, never of the
  vault;
- **incremental equals rebuild** — incremental maintenance equals a from-zero rebuild over
  the same tree;
- **the work bar** — the work a write does follows its blast radius, never the vault's size.

**Forbidden fan-out** is propagation that is unbounded or unindexed: it reaches a set no
bound limits, or it reaches its set other than through an index seek. Unbounded propagation
fails the work bar and holds the changeset's transaction and memory past its blast radius;
unindexed propagation fails the work bar. Nondeterminism and model-version dependence fail
incremental equals rebuild. A projection that carries any of them cannot ride the changeset
at any price.

## Link health is re-decided inside the changeset

**Link health** is a validate finding family with four kinds, each a finding about exactly
one link, and it follows the addressing and eligibility rules every surface already reads a
link's health by:

- a link addressed elsewhere — a scheme link, a network link among them — is not judged and
  raises nothing;
- an eligible link that resolves to no document and no vault file is **broken**, whatever
  extension its last segment carries ([ADR 0040](0040-a-link-resolves-as-obsidian-resolves-it.md));
  a link that resolves to one vault file is healthy, and its anchor is not judged;
- an eligible link that resolves to two or more documents or vault files is **ambiguous**, and its finding
  carries a bounded head of five candidates, the exact total, and a hint that names how to
  enumerate the rest;
- a link that resolves to one document lacking the link's `#heading` or `#^block` has a
  **missing anchor**; a link that resolves to one broken document
  ([ADR 0046](0046-one-unreadable-path-never-breaks-the-vault.md)) is not judged for its
  anchor. Every broken document holds names alone, whether read before or never read, so
  its headings and blocks are unknown rather than absent. An absent body is not an empty
  one: an empty readable document holds a content hash and an empty body, and an anchor on
  a link to it is missing. The predicate tests `body IS NULL` by one primary-key seek per
  judged link and target pair, never a count of zero headings and blocks, which a readable
  document can hold too;
- a path-addressed link beneath a directory marked unreadable
  ([ADR 0046](0046-one-unreadable-path-never-breaks-the-vault.md)) that resolves to no held
  name is not judged: its target is unknown rather than absent. A link resolving to a held
  name resolves as any link to that name does. Its anchor is unjudged where the names-only
  row holds no body. A suffix-addressed link cannot be tied to one directory, so it is judged
  over the names the store holds, and the directory's finding marks the gap those names may have.

A link written through `vault://` that resolves to one candidate carries a **vault-scheme**
finding unless its missing anchor takes precedence.

An embed is a link and carries the same kinds. Every link-health finding has warning
severity.

Each finding's truth depends on documents the link is not written in: which documents its
target names, and what headings and blocks the one it names holds. Creating, deleting,
renaming or moving a document, and editing the headings or blocks of a document a link
names, can each change a finding about a link in a document nobody touched. Its inputs are
lane-1 pillars — stored links, headings, blocks, document keys, and the unreadable-directory
findings the host files — in the same way the tag facet's findings read stored tag rows, so
it is not a projection over another index's output and the inference firewall below applies
to it unchanged.

Five requirements bind these findings. A validate is a pure read over the lane-1 findings
pillar: it runs no rule and derives nothing. Findings are one of the four protected surfaces
behind the correctness boundary
([ADR 0024](0024-search-enhancement-is-a-query-surface-tier.md)), so a link-health finding
must be exact for the snapshot it is read under, not eventually right. The regression
registry states the rest:

- `derived-findings-are-materialized-and-maintained` — "a warm validate reads findings and
  derives nothing";
- `incremental-equals-rebuild` — the maintained index equals a from-scratch rebuild,
  "including finding order";
- `cost-is-independent-of-vault-size` — per-operation cost is a function of "the changed set
  plus its link neighborhood", so the link neighborhood is already priced as part of a
  change's blast radius.

**A finding is discarded by the act that re-decides its link.** Inside its own transaction a
changeset discards every finding standing at a path it writes or kills, every finding filed
under the class key of a path whose names it moved, and every finding keyed by the exact path
of such a path, and then re-decides the links those findings were about beside the links the
re-decided set names. An in-place rewrite that moves an anchor value discards the standing
finding of every link it re-decides for that move: until the anchored-link index is built, by
the same class and exact-path discard a names move runs at the written path, and once it is
built, by each re-decided link's own place. Any other finding stands, because nothing its
verdict reads moved. The class discard predates ADR
0021, which neither admitted nor forbade it; the path-key axis beside it, and the re-decision
that files the findings again in the same act, were ruled by ADR 0027 and are built.

**A link reaches documents through one of two key spaces, and they are not
interchangeable.** A suffix-addressed link — `[[glossary]]`, `[[norn/glossary]]` — is keyed
by the classes its reductions name, and a document is in such a class by its suffix. A
path-addressed link — `[x](./dir/t.md)`, `[[vault://Notes]]` — is keyed by the exact paths
it spells, and no class range reaches it: deleting `dir/t.md` changes the class `t/`, and the
path key `dir/t.md` is not in that range. An unprefixed target in either family, `[x](dir/t.md)`
among them, is suffix-addressed (ADR 0040). A change reaches a link's finding only through the key
space the link is addressed in.

**The decision.** The store judges link health in SQL and files those findings inside the
changeset, over per-document facts the host derives from the text layer — link targets,
headings, blocks, and the keys a link is read through. The anchor rule is a predicate over
readings stored per heading, per block and per link, so the whole judgment is one SQL
predicate. The findings are judged against the facts the transaction holds once every entry
of the changeset is written. The exception admits exactly the link-health family: broken,
ambiguous, missing anchor and vault-scheme, each a finding about exactly one link, filed to the findings
pillar. It is never an aggregate over the set, and never a write to another pillar. A
from-zero build is the one write that judges the same family by the same predicate at
another time, as the next section rules.

**The re-decided set** is the union of:

- every link held by a document the changeset writes;
- every suffix-addressed link whose keys fall in the class of a path whose names the
  changeset moved, as the next section defines;
- every link whose standing finding is filed under such a class — a finding is filed under
  the naming class of each candidate it carries, so a link addressed by path whose candidate
  is named by its minimal disambiguating suffix is reached when a name in that class moves;
- every path-addressed link whose key equals a path whose names the changeset moved,
  including the old path of a rename;
- every anchored link whose keys reach a document whose anchor values an in-place rewrite
  moved, and that names one of the values moved there, as the next section defines — a
  superset of the links resolving to that document alone, which the superset rule admits;
- every path-addressed link whose key lies beneath a directory whose unreadable-directory
  finding the act files or ends, re-decided in the transaction that files or ends that
  finding, so a link left unjudged under a marked directory is judged once it lists again.
  That act first discards the findings already keyed beneath the directory, by a prefix
  range over path keys, and then files again: the equality discard on a written or killed
  path does not reach a key beneath a directory.

## A write re-decides only the links whose inputs it moved

**A link's verdict reads two things outside the document that holds it.** Its broken,
ambiguous and vault-scheme verdicts, and the candidate head and minimal disambiguating
suffixes a finding carries, read the **names** its keys reach: which document and vault-file
rows stand at the classes and exact paths it is keyed by, and in the naming classes of the
candidates it carries; the facts each row is matched by — its path, whether it is a document,
a vault file or a names-only row, its suffix and path keys, and its admitting count, the
fewest target segments that admit it, which the schema's `paths.ambiguity_ignore` globs set;
and which directories along a spelled path are marked unreadable. Its missing-anchor verdict
reads the **anchor values** of the one document it names: the readings and slugs of that
document's headings, the identifiers of its blocks, and whether its body is known at all. The
anchor predicate is an equality over those values — a heading matches a link's anchor text or
marked reading by its reading, or the anchor as written by its slug, and a block matches a
block reference by its identifier — so, while the body stays known, whether a document holds
the place a link names changes only when a value equal to one the link carries enters or
leaves the document. Nothing else outside the holding document enters a verdict: a finding's
message, target, span and ordinal are the holding document's.

**Exactly one kind of entry is narrowed: an in-place rewrite.** An entry is an **in-place
rewrite** where it writes a readable document at a path that held a readable document before
the entry, and the row it writes matches the row it replaces in every fact a row is matched
by — its path byte for byte, its kind, its suffix and path keys, and its admitting count; the
row keeps its identity. A respelling that only a folding comparison calls the same path is a
death and a birth. Every other
entry **moves the names** at every path it reaches and re-decides by them, as ADR 0044 ruled:
a birth, a death, a rename (a death of the old path beside a birth of the new), a vault
file's birth or death, a provisional name written during a from-zero build's names step, a
document that becomes unreadable and keeps its names alone
([ADR 0046](0046-one-unreadable-path-never-breaks-the-vault.md)), a names-only row written as
a readable document, a range beneath an unlistable directory, and a write whose row differs
from the one it replaces in any fact a row is matched by. An entry whose effect on those facts
cannot be read from the entry and the row it replaces is taken to move them. Some of these
leave the names standing and move only the body between known and unknown; re-deciding them
by their names is the superset the next paragraph but one admits, and it reaches every
anchored link the move can flip, including one naming a value neither side holds.

**An in-place rewrite moves the anchor values** it changes: the heading readings, heading
slugs and block identifiers the document holds after the write, set against those it held
before. The comparison reads only the written document's own rows, before the write discards
them and beside the replacement it writes, so its cost is the written document's and never
the vault's. **An in-place rewrite that moves no anchor value re-decides only its own links.**
Its path stays where it stood, every link in another document reads the same inputs before
and after, and that link's standing finding, keyed by classes and paths whose names did not
move, stays: it is exactly the finding a re-decision would file again. The subject discard
still takes every finding standing at the written path, and the re-decision files the
written document's own links again.

**Re-deciding more than the re-decided set files the same findings.** A link whose inputs did
not move is re-decided to the finding it already holds, so a changeset may re-decide a
superset of the set and stay exact; the narrowing is an obligation on the work, which the
write-work bar measures, and never a condition of correctness. **The index that reaches an
anchored link by the anchor value it names is not yet built.** Until it is, an in-place
rewrite that moves an anchor value re-decides every link reaching the written path's class
and exact path, as ADR 0044 did for every write, and the write-work bar holds such a rewrite
to that reach and every other in-place rewrite to its own links. A seek by anchor value alone
across the vault is not that index: a common heading names links to many other documents, and
their work would follow the vault rather than the write.

**The job-end prune's transaction is a lane-1 re-deciding transaction.** Where a walk's
job-end prune ends an unreadable-directory finding
([ADR 0046](0046-one-unreadable-path-never-breaks-the-vault.md)), the transaction that ends
it discards and re-decides the links beneath that directory, under the five conditions
below as a changeset is: the finding's end and the re-decision commit together or not at
all, and the transaction's work is bounded by the re-decided set.

Every link-health finding carries invalidation keys in the key space its link is addressed
in, and the naming classes of the candidates it carries, so the changeset's discard reaches it
through the same keys the re-decided set is found by. Within that family, the exception admits only re-decision that is:

1. **indexed** — every link in the re-decided set is reached by an index seek: a written
   document's links by that document, every other link by its stored keys in its own key
   space — an anchored link reached for a moved anchor value by its keys and that value
   together once the index holding both is built, and until then by its keys alone, as every
   link reaching the written path's class and exact path — a link reached through a
   candidate's naming class by the finding filed under that class, and the links beneath a marked directory by a range seek over path keys under the
   directory's own key. A path-addressed link reaches a marked directory the same way: each
   directory its spelled path passes through is a prefix of that path, and the place key of
   each is sought among the unreadable-directory findings, one seek per directory the spelling
   names, so the work follows the spelling, never the vault;
2. **keyed like its discard** — every finding it files is keyed by exactly the class and path
   keys the discard ranges over, so the next change that moves the names under any of them
   discards it, and the act that re-decides its link for a moved anchor value replaces it;
3. **deterministic, in facts and in order** — the same facts yield the same findings, and the
   order of a path's findings is a function of the findings' facts — the link's position in
   its document, then the kind — never of when they were filed;
4. **over facts the transaction that re-decides reads** — per-document facts lane 1 derives,
   and no lane-2 or engine output;
5. **bounded by the re-decided set** — its work is the links in that set plus the candidates
   they resolve against and the directories their spelled paths name, with each distinct
   target key resolved once.

The five conditions are necessary, not sufficient. Backlink counts keyed by class, a rule
that a linked document carries a tag, and tags inherited along links would each meet them;
none is admitted. Any other bounded, indexed cross-document propagation needs its own
ruling.

**The admission carries a write-work bar.** A test holds that the work a write does
re-deciding link health follows the re-decided set and the candidates it resolves against,
not the size of the vault, and it varies candidate multiplicity independently of in-link
count, so an ambiguous finding's exact total cannot hide vault-sized work. The bar is what
indexed access is for: a seek is the only access that keeps the work within the bound, and
the bar is what makes condition 5 an enforced property rather than a promise. The bound is
not small in every vault. A neighborhood can be most of the vault: a hub stem that most
documents link to.

**Every schema pin owes the vault a link-health re-decision over every link.** Link-health
findings are keyed by the vault schema fingerprint, as every finding is, and a pin discards
every finding keyed by the fingerprint it replaced — whether or not the schema changed
anything link health reads. The walk after a pin discharges it by deriving every row
stamped at or below the pin's generation again, under every schema: the changeset that writes
a document re-decides the links it holds, and a row whose admitting count the new
`paths.ambiguity_ignore` globs change is not an in-place rewrite, so it moves its names and
re-decides every link its class and path reach, including one a chunk already judged against
the old count. The obligation's cost has a floor: every link in the vault is re-decided,
chunked as a heal is. Link health reads only stored per-document facts, so the re-decision
needs no document to be read again; the walk that re-reads every document pays a heal's cost
on every pin as well. A mechanism meeting the floor is not built; schema edits are rare, and
exactness is kept.

**The store's contract changes at one point.** Before ADR 0027 the store recorded only the
findings the host handed it with a changeset, and decided none. Link-health findings are the
first it judges and files itself; every other finding stays the host's to decide.

## A from-zero build judges link health once

**The scope is every from-zero build.** A from-zero build constructs a canonical store from
empty: a vault's first build, and every rebuild of an existing store from zero — over a DDL
fingerprint, derivation version or path order the store does not match, over damage, or by
any other rung that discards the database and derives the vault again. No reader can adopt
the store as complete until its construction ends: the store carries an under-construction
mark until then, and an open that finds the mark rebuilds from zero rather than resuming
over what a crash left, as ADR 0045 ruled. A full tree heal of an existing store, a recovery
that does not rebuild from zero and every write after the build ends maintain an existing
store; each keeps the per-changeset re-decision above unchanged. A schema pin keeps the
obligation ruled above, whose mechanism stays open; that mechanism may reuse the single
judgment's routine.

**During a from-zero build no changeset re-decides link health.** Once the structure facts
of every document — its headings, blocks and links — have landed, the store judges every
eligible link once, plus the links a requeued path reaches, in path order, in bounded
chunks; a requeued directory that lists again reaches the links beneath it, and its
unreadable-directory finding ends before they are judged, as ADR 0045 ruled. The judgment is the one above:
the same SQL predicate, over the same summaries of the keys each link is read through,
filing the same kinds with the same invalidation keys, in the same deterministic order. A
path a filesystem fact requeues while the judgment runs is derived again first, and the
judgment then judges again the links that path reaches — the links it holds, every link its
class and exact path reach, and every link whose finding is filed under its class, as a
changeset that moves that path's names re-decides — before it ends, so every finding it files is judged over the
facts the store holds when it ends. Facts accepted after the judgment ends are reconciled
before `Ready` as ordinary changesets, under the per-changeset rule. The judgment's working
memory is a bounded chunk of links and the candidates they resolve against, plus one count
per distinct key naming more than a candidate head.
The finished store's findings equal the findings per-changeset re-decision files over the
same tree, finding order included, so incremental equals rebuild holds for every store a
reader can adopt, apart from the accepted delay in refreshing a still-broken document's
cause (ADR 0046). Every broken document has the same names-only state in a maintained
store and a build from zero, with an unreadable-document finding. The exception ADR 0046 carves is beneath an
unlistable directory: a maintained store keeps only names, ids and keys beneath it, where a
build from zero holds none. Byte-derived facts and content-side findings are cleared in the
transaction filing the directory's finding. That finding marks the gap until it lists again.

**The four obligations hold for the pass as a from-zero build has them.** Atomicity protects
a reader from a tear between a row and the findings judged over it. A from-zero build admits
readers of the rows it has completed, but it withholds the findings surface, not the store,
until the judgment has run: no reader reads a link-health finding before then, and an open
after a crash before the transaction that ends construction commits discards the
construction whole. Working memory is bounded by the chunk and its candidates, plus one
count per distinct key naming more than a candidate head. The
work bar measures a write against its blast radius, and a from-zero build's blast radius is
the whole vault: judging each link once is the least work that blast radius admits, where
per-changeset re-decision exceeds it.

**A from-zero build may defer judgment because nothing during the build reads the
findings.** A link's broken or ambiguous verdict reads only names — which documents its keys
name, and which directories along a spelled path are marked unreadable — and the vault
schema's `paths.ambiguity_ignore` globs; its missing-anchor verdict reads only the headings
and blocks of the one document it names. The store is first a
faithful copy of the files, and link-health findings serve validate and repair. Stored
link-health findings are read only by the surfaces that report them — validate, a find's
`HasFinding` part and a findings column — and by repair, which plans from them; no other
query's answer reads them, and no write's safety does. A write other than a repair judges
the health of the links it changes itself while it plans, and resolution reads names and
stored link rows, never a finding. A reader that meets a from-zero build before its judgment
has run sees link health as still loading, never as absent, and a `HasFinding` part is
refused as not ready, as ADR 0045 ruled.

**What it buys.** Measured on a lab prototype in release builds, on a 10,014-document
fixture, per-changeset re-decision during a from-zero build made 89,186 link judgments for
59,969 links and wrote 32,874 link-health findings, most of which later changesets
discarded. In a phase profile of that build alone, link health was about 9.3 s of about
23.6 s; that profile measures construction, not the end-to-end time to `Ready` ADR 0045
reports for a build that also served early reads. Judging once over the same vault,
with no fact accepted during the build, made exactly 59,969 judgments, one per link, and wrote
4,001 findings, the findings the finished store holds.

## Lane 2 reads change through a feed

This section is amended. A cursor is now also valid only while its consumer has read through
the death floor, and the rejected retained change log is restated.

Lane 2 consumes change through a feed that is a query, not a structure: current document
rows and tombstones, ordered by the store's global write generation, projecting content
fingerprints so a consumer triages before it fetches. Cursors are consumer-owned and valid
within one store epoch, and only while their consumer has read through the store's death
floor, the highest generation of any death the death prune has removed
([The death floor](#the-death-floor)). Either failure means a reconcile against current
state and a rescan, and content-addressed sidecar rows make a rescan recompute only what
actually changed, so expensive derived state survives the cheap state's rebuild. The host
relays a contentless wake; a missed wake costs latency, never correctness.

A retained change log with registered subscriber cursors was rejected. Such a log keeps
every change until its slowest subscriber has read it, so the slowest consumer pins retention
without limit, which forces an eviction policy, and eviction forces consumers to
resynchronize from current state — which is what the feed already is. The feed is not free
of retention: the store keeps a death that lane 2 has not read. But lane 2 pins it only up to
a bound the vault sets, one unread death per live document, and the store registers no
subscriber: the host hands the death prune how far lane 2 has read as an argument to one
unit. Past the bound the death prune removes the oldest unread deaths and raises the floor,
so the eviction it does make becomes a reconcile from current state rather than lost data.
Cross-file transactional atomicity is not required, because eventual consistency is lane 2's
stated contract.

## Inference never re-enters correctness

Inferred and higher-order derived state — any projection whose inputs include a model or
another index's output — is **query-surface-only**. It never becomes a finding, a plan, or a
repair input. This generalizes the boundary invariant that keeps `norn-embed` blind, and it
is carried the same way: absent crate edges, and a store read surface partitioned so the
correctness path has no API that exposes inferred pillars. A typed advisory finding class
for inferred results was considered and deferred: adding it later is additive, while
withdrawing inferred rows from the table the applier trusts would be nearly impossible.

A projection over named lane-1 pillars is not a projection over another index's output: the
tag facet's findings read stored tag rows and link-health findings read stored links,
headings, blocks and the unreadable-directory findings the host files, and the firewall
stands whole over both. A from-zero build's single
judgment reads the same pillars and nothing else.

## Invalidation keys

Every projection names one invalidation key, and the key lives with the state it
invalidates: the vault schema fingerprint for lane-1 schema-keyed tables, an engine's own
configuration and model versions for its sidecar. Link health's findings carry that
fingerprint, which any pin moves, and beside it the class and path keys the changeset's
discard ranges over for the re-decided set. A finding a from-zero build files carries the
same keys, so the first write after the judgment discards it exactly as it would discard a
finding an ordinary changeset filed. How invalidated state converges is the owning side's judgment.
Wholesale rebuild is the current implementation and the permanent always-correct floor;
finer responses — adding one index without a rebuild, a pillar carrying its own version
key — are carved evolutions taken when something like measured cost forces them, not
obligations.

## Alternatives

**A lane-2 link-health engine over the change feed.** The inference firewall forbids it: a
finding filed from an engine's sidecar is a projection over another index's output. It is
also only eventually consistent, so at any instant between a write and the engine's drain
the findings differ from a rebuild's, which fails `incremental-equals-rebuild`. It needs a
sidecar database, a new crate, and a feed handle wider than the one an engine holds. And a
find that asks which documents hold a finding could not join the sidecar in one snapshot.

**Judging link health at read time, in validate, through SQL joins.** An unnarrowed page
would step every healthy link to find the unhealthy ones, which fails size independence. A
part that drives a read by "holds a finding" would no longer seek what it keeps, which fails
`narrowing-arguments-narrow-work`. An anchor rule judged per read would either join every
link to its target's headings on every page, which is the unnarrowed cost again, or run as
an in-memory post-pass over more rows than the read returns, which fails
`no-in-memory-query-layer`. And maintaining every input while deriving the findings on each
read is the exact defect `derived-findings-are-materialized-and-maintained` names.

**Moving link health off the changeset into a deferred drain.** Only the surfaces that
report health read its findings, so a write could record the links it owes a verdict and a
background pass could judge them behind a link-health status, as a from-zero build already
reports one. That takes every cost out of the write, a hub's birth included, but findings
would then be eventually consistent between a write and the drain, which the exactness this
decision keeps forbids, and validate would have to wait on or run the drain. It needs its own
decision and a drain, which belong with a host-wide background queue; a deferred drain would
want this decision's narrow set too, so this decision does not foreclose it.

**Re-deciding every anchored link reaching a document whose anchors moved.** Simpler, and an
admissible superset, but it prices a glossary's new term at every anchored link to the
glossary. The anchor predicate is an equality, so the set of links whose verdict can change is
exactly the links naming a moved value, and that set is what the work is held to.

**Re-deciding per changeset during a from-zero build, as ADR 0027 ruled.** Each chunk
re-judges every class its names touch, against names later chunks have not yet written, and
files findings later chunks discard, at the cost measured above. The repetition buys
nothing, because no reader reads a finding before the last chunk lands.

## Consequences

**A hub stem costs its in-links when its names move, inside the changeset's transaction.**
Creating, deleting or renaming a document whose stem many links name re-decides all of them
in that write: adding one
`index.md` to a vault with 5,000 `[[index]]` links re-decides 5,000 links before the
changeset commits. The re-decision runs inside the changeset's transaction on the store's
one writer connection, so watcher catch-up and every other write on that store wait for its
duration. That stall is an accepted cost.

**Memory must stay bounded while the transaction is long.** The bounded-working-memory
obligation requires the re-decided set to be judged in the statement and streamed out of it,
never materialized beyond a bounded chunk of rows, so that a hub costs time inside the
transaction and not memory; a memory bar is what tests it. A from-zero build's single
judgment is held to the same bar: a bounded chunk of links and the candidates they resolve
against, plus one count per distinct key naming more than a candidate head.

**A full tree heal of an existing store still re-decides common-stem classes once per
chunk.** A heal that maintains an existing store — an attach over a store it adopts, or a
recovery that does not rebuild from zero — commits in chunks under the per-changeset rule,
and a class in which many chunks move names is re-decided by each of them. Such a heal still
converges to the rebuild's findings; the price is repeated work on common stems, largest
when the heal reaches most of the vault.

**Link health reads as still loading until a from-zero build's judgment has run.** Its
findings exist only once the single judgment has run, so every surface that reports them
answers "still loading" until then, for a rebuild of an existing store as for a vault's
first build, as ADR 0045 ruled.

**An ordinary edit to a hub costs the hub's own links.** A write that changes a widely linked
document's body — an index, a map of content, a daily note — and leaves its names and anchor
values alone re-decides only the links it holds, however many links name it. A write that
adds, removes or renames a heading or block id there costs, until the anchored-link index is
built, every link reaching its class and path, as every write to it did before; once built,
it costs the links naming a value it moved. A glossary that 2,000 `[[glossary#Term]]` links
name pays for the links naming the term it changed.

**Every in-place rewrite pays a bounded comparison.** A readable document rewritten in place reads its
old heading readings, slugs and block identifiers and compares them with what it writes, a
cost that follows the written document. A write whose names move skips the comparison: it
re-decides by its names regardless.

## Context

A complete requested document can be derived before every document in a vault.
Complete metadata can answer field and path queries before full rich derivation.
Making every query wait for global readiness couples simple reads to unrelated
construction. Answering from arbitrary partial rows instead would give false
absence, incomplete content, or incorrect resolution.

ADR 0043 bought its early answers with disposable sources: a names catalogue, a
metadata store and per-request Get projections, each derived beside the canonical
store and discarded after it. Every document an early source served was derived
twice, and the extra work moved whole-vault readiness later. A store built in steps
holds the same partial facts in the place they stay, so an early answer reads the
canonical store and no fact is derived twice. What keeps such an answer exact is the
rule ADR 0043 already stated: availability depends on proven completeness of the data
a request reads, never on a percentage of files processed or an optimistic readiness
label.

The reader-capacity rationale from ADR 0042 also remains:

One connection per entry serializes independent reads before SQL can run. Fixed
pools per vault multiply idle caches and descriptors by the number of attached
vaults. Expected traffic is bursts across few vaults, with occasional sustained
concurrency. Capacity therefore follows active demand within one host-wide bound.
Independent pages of one immutable count result have no SQL dependency on each
other; a lock around its sole anonymous connection was an incidental storage limit.
Concurrency is required here without waiting for a contention threshold.

## Source reader acquisition

The store's one writer connection sits behind `&mut`, inside the attachment that every
lifecycle job holds for its whole duration — so a wire read borrowing it would serialize
behind every warm job that holds the store while trust stays `Ready` (polls, maintenance,
plan application), and the timing-barred query shapes would measure orchestration where the
acceptance contract bars SQL. Canonical acquisition is unavailable until a from-zero build
publishes `Ready`; a canonical read meeting an entry under a from-zero build refuses at
once, and a capability that build has published may answer earlier, from the store under
construction, under the rules below.
Every later heal takes trust out of `Ready`. A read waits out the heals that take in a
change — a watcher batch's reconcile and a vault schema reload's — and refuses at once
during a recovery that follows withdrawn trust without rebuilding from zero; a rebuild from
zero is a from-zero build and serves reads as one. A shared
borrow of the live store cannot serve reads either — a `&self` cannot exist while a job
holds the attachment mutably. Wire reads therefore run on pooled read-only handles
`norn-store` mints from a live `Store`, a store under a from-zero build included.
Minting a pool opens no database and runs no SQL. Connections open lazily outside
the entry gate, read-only with `query_only` set, carrying the read builders and their `EXPLAIN` seam
so the gates exercise the connection reads actually use, and keeping the
no-connections-outside-store rule whole. The host holds the handle in entry state **beside
the attachment, never inside it**.

Each source read is one WAL snapshot transaction, **established under the entry gate lock in the
same critical section that reads trust** — established by a read statement, because a bare
deferred `BEGIN` takes no snapshot — so the trust label and the snapshot describe the same
instant; the read itself runs outside the lock. Under the gate, acquisition tries
only for an immediately available warm grant. It never waits for budget capacity
or opens a connection there. Otherwise it gives back the gate, acquires its complete
grant and opens any missing source connection outside the gate, then retakes the
gate under the same deadline. That retake rechecks the pool identity and current
stance before establishing. If the pool was replaced, the read returns the grant
and acquires the current pool within its original bound. If the entry settles, it
waits under the stance below. If the entry neither settles nor serves, it refuses
with the published cause. Capacity may be held by reads of other entries, so
reader-unavailable describes a serving entry whose read resources could not be
acquired within the bound, whether or not its own pool has active readers.

ADR 0015 priced that contention as "the hot per-entry lock riding across that one
statement — a cheap read alone, a wait where a concurrent read still holds the one reader",
and it priced it wrong. A lock held across a wait for a resource that only another holder
of the same lock can release is not a slow path: one read in flight and one concurrent
acquisition freeze every gate-taking surface of that entry — status, demand, the reap loop
and job-leg completion among them — with no mutation and no second vault needed to reach
it. That price is withdrawn. What contention costs instead is the second reading a
contended acquisition takes, and what the withdrawal keeps is the coupling itself: every
snapshot is still established under the entry gate, in the same critical section that reads
the published demand, so the state a read answers under and the snapshot it answers from
still describe one instant. The uninterrupted critical section per read that the old
pricing bought was buying the deadlock.

A snapshot sees the last committed increment, never a torn one,
never blocks the writer, and may trail in-flight derivation; trust state, not the
connection, is what buys the right to answer. Concurrent reads acquire independent
connections through the same store-owned pool. They may wait for bounded capacity;
no application lock spans their SQL execution. A reader never concludes a heal rung: its open bypasses inspection and rebuild, and
`query_only` makes derivation impossible by construction, which turns the warm-read
zero-derivation-counter bar structural.

The lifetime rule is carried by one seam. A pool is installed beside coverage under
the gate that publishes trust. A read opens or acquires a connection outside that gate,
then checks the pool identity, stance and acquisition deadline again before establishing.
Every controllable acquisition wait, including SQLite busy waits during a cold open
and snapshot establishment, consumes the original remaining deadline. Every borrower
sets the connection's busy timeout to its own remaining deadline before `BEGIN` and
before the first read, including when it reuses a warm connection. An exhausted deadline refuses as
reader-unavailable before snapshot establishment after that acquisition wait. Synchronous filesystem calls cannot
be cancelled; if one returns after the deadline, acquisition refuses without establishing.
An open remains charged until its resources close, including an open that finishes late.
An open takes no snapshot: a write between opening and establishment is therefore read
at establishment. A connection opened for another database epoch is refused. Damage met during open
or establishment enters the same damage judgment as damage met during query SQL,
including carrying that verdict to a running lifecycle claim. It schedules the
existing rebuild path, never a heal inside the reader. Environmental open failures,
file-descriptor exhaustion, memory exhaustion and busy failures remain reader-unavailable
and schedule no rebuild. A later read can retry an environmental failure.

Retiring a pool prevents new acquisitions and wakes its waiters. Teardown drops ownership
without waiting for an open, a checked-out connection, or a query. Idle connections close
outside the entry gate; checked-out connections close when their reads finish. A read
waiting on a replaced pool acquires the current pool under its original deadline and
answers only where the current stance permits it.

**A read's hold is a demand lease, taken under the same hold of the entry gate that reads
the published demand and clones the handle.** What the lease buys is what it buys every
other caller: it holds the entry's idle interval open for as long as the read runs and
restarts it when the hold drops, it clears the idle deadline, it withdraws an idle detach
that is scheduled and not yet in flight, it raises the recovery the entry owes and gives
that demand back with the hold, and where the entry is free to run it, it schedules the
work the entry owes, read as a chain: the attach where the entry holds no coverage, and
under that the rebuild it owes, the recovery beneath that, and the reconcile where it owes
neither — with the read answering under what that work publishes, and under the state it
found where the work publishes none. The one recovery a read's demand neither raises nor
schedules is a recovery only a change to the vault can answer, while nothing has changed
since it failed, as ruled below. A read asks for the owed recovery because what a read wants from an
untrusted vault is exactly that it become answerable again; a read workload that never
healed the vault it reads would be a dead end. So an idle teardown neither runs under a
read nor precedes one into the entry, and a workload of reads alone keeps an attached vault
attached and an untrusted one converging wherever a recovery can make it converge. **Teardown never waits for a read**, and the
lease changes nothing about that: a refusal or a destruction moves first and consults no read, and a job leg failing its way
into a release consults none either. Through any of them the read keeps answering from the
handle it holds and from nothing else — no move states that the file behind that handle
outlives the teardown, and nothing pins one that would. The read path states that absence
rather than leaving it open: a read that a teardown runs under is answering against files
the teardown may already have unlinked. The pin stands beside the lease and keeps its own
narrow job, which is to say that the entry is held by work outside its lock.

**A read's demand differs from every other demand in two steps: it withdraws no park, and
it does not restart a recovery only a change to the vault can answer.** The first is ruled
here and the second in its own section below. An ordinary demand retires the registry's parks — a duplicate root, a root the
registry could not read — where maintainer contention is not already answering it, because
withdrawing them is how a caller asks for the acquisition that classifies those roots
again; a contended entry keeps every park it stands on, since no acquisition follows a
demand the contention answers. A read asks for no such acquisition under any condition; it
asks for an answer, and a vault the registry has parked has none to give. A read against a parked
entry therefore refuses with the park's own code — identity refused, duplicate root and
maintainer contention alike — and schedules nothing.

## A read does not restart a recovery only a change to the vault can answer

**A recovery only a change to the vault can answer is the one owed where the entry's own
declaration withholds trust**: the coverage stands, the ops say nothing may be derived under
the declaration it read — a vault schema this build cannot read — and the entry publishes
that cause as untrusted and owes the recovery that reads the declaration again. An attach, a
recovery and a rebuild each end that way over such a declaration. Reading the same bytes
again reaches the same verdict, so a recovery restarted with nothing changed is work that
cannot succeed.

**Its cause is unchanged while the entry has taken in no fact since the attempt ended.** The
entry already keeps its position in the stream of facts it takes in, and the attempt that
failed records that position where it publishes its cause. A read's demand compares the two,
and while they are equal it raises no recovery and schedules none: the read is refused as
`host/entry-untrusted`, carrying the cause. No reconcile runs over an entry that owes a
recovery, so a watcher fact is the one thing that moves the position there. Any fact moves
it, not only a fact about a control file: editors replace a control file by writing a
temporary file and renaming it over the original, so a filter on the file's own name would
under-report the correction, and the recovery an unrelated edit restarts is the priced cost
of over-reporting. An attempt that
itself took in a fact while it ran records nothing, because what it read may be older than
that fact, and the next read's demand asks for the recovery again.

**A change to a control file is a fact to such an entry.** Watcher facts about the two
control files are otherwise discarded before they reach the lifecycle, because an explicit
reload is what replaces an active declaration. An attachment whose declaration withholds
trust has no active declaration to replace, and a changed control file is exactly the change
that can heal it, so over such an attachment a control-file change — or a rescan of the
schema source, which the watcher reports where the folder a vault schema outside the vault
sits in is replaced — reaches the lifecycle as a fact. It is inert to a reconcile, which
derives documents and reads no control file.

**Every other demand is unchanged.** A client's demand still raises and schedules the owed
recovery whatever the position says, because a client asking is itself the evidence that the
attempt is wanted. Every other recovery a read meets — for an environmental refusal or a
lost watcher — is still restarted by a read's demand,
because its cause can heal outside the vault's bytes, where no fact the entry takes in would
say so.

### Known limit

A vault schema whose `schema_source` lies outside the watcher's coverage yields no fact
when it changes, so a read goes on answering its old cause; only a client's demand, or a
reload, reads it again and heals it.

## A read waits for the facts it met

**Whether a canonical read waits is its stance, and the stance is read under the entry
gate, in the same critical section that reads the published demand.** There are two
canonical stances. Capabilities a from-zero build has published additionally use the
admission below; they do not turn an entry under a from-zero build into a canonical
settling entry.

- **Settle.** An entry that entered warming from `Ready`, with no withdrawal of trust in
  between, settles, and a read that meets it waits. That is an entry taking in a change: a
  polled watcher batch, a reconcile turn, or a vault schema reload.
- **Refuse.** Everything else refuses at once, with its reason: an entry under a from-zero
  build unless this request's capability is admitted below, an entry that lost trust —
  a watcher overflow or loss, an environmental refusal,
  damaged derived state — a release, a detach or a drop, and every park. Warming entered
  from untrusted by a recovery that does not rebuild from zero refuses at once, although it
  too runs over coverage that was once `Ready`. Warming entered from untrusted by a rebuild
  from zero admits what that build has published, as a vault's first build does.

Warming published by a reconcile and warming published by a recovery or a rebuild carry the
same trust label, so the label does not say how warming was entered. The entry's own state
holds that fact, beside the published demand, and the stance is read from it.

**The wait obeys the standing rule: no acquisition waits while it holds the entry gate.** A
settling read gives the gate back, waits outside it on a signal that moves when the stance
changes, when the entry has derived further through the facts it has taken in, or when a
reader is installed over a settling entry, takes the gate again, and reads the published demand afresh.

**A settling read waits for the facts it met, never for the ones that arrive after it.**
Under the first hold that finds the entry settling, the read records how far the facts the
entry has taken in run at that instant, and it never moves that record. It establishes its
snapshot, in the same critical section that reads the published demand, at the first of two
things it observes: the entry `Ready`, or the entry having derived every fact up to that
record with a reader standing beside it. Either way the snapshot holds every fact the entry
knew of when the read met it, so **it is never older than the change the read arrived
during**. A vault schema reload replaces the pool: its activation closes the reader, and the
leg that activates installs another as it ends. A read that first meets the entry while the
reader that activation closed is not yet replaced records, in place of the instant it met it,
how far the facts run once that leg has taken in its end drain. Its position then lies past
every edit the watcher held when it arrived: that drain takes them in, and where the drain is
saturated, the intake that derives through the position drains first. A read it settles answers from that pool once the facts
it met are derived, as the section
[A schema reload serves reads again when its activation ends](#a-schema-reload-serves-reads-again-when-its-activation-ends)
records. **A settling read that finds a
new reader after the wait acquires that reader and answers from it, under the same bound**;
it refuses only where the entry neither settles nor serves — lost trust, a release, a park.
The demand it recorded before the wait holds the entry across it, as it does across a wait
for the connection.

**The answer reports what it was taken under.** The trust an answer carries is what the
entry published at the instant its snapshot was established. Where later facts are still
being derived, that is warming in its healing phase, not `Ready`, and the answer says so.
The generation it carries is the store's committed write generation.

**The wait is fair.** It never runs until the vault falls quiet: a vault edited faster than
one reconcile turn takes still answers a read that meets it, once the facts that read met are
derived. Past its bound, the read refuses as still indexing, under the `host/entry-not-ready`
code, with the message "this vault is still indexing a change".

**Teardown never waits for a read, and a waiting read is no exception.** A teardown's
publication moves the stance to refuse, which wakes every waiting read, and each of them
takes the gate again and refuses with what the entry then publishes, unless the entry has
reached `Ready` again before that retake.

**The bound is operational containment, not a performance threshold.** It keeps a read from
waiting on a change that does not converge, and it states nothing about how fast a change
should settle. It is a lifecycle policy value, and its production value equals the settle
ceiling already authored for one vault walk: 5 seconds. The bound runs from the read's
first take of the entry gate and covers every wait the read takes: that first take, the wait
for the shared budget, connection opening, snapshot establishment and the change together,
and the retake of the gate after
each. **A take of the entry gate past the bound refuses the read: the first take as
reader-unavailable, before the read records any demand, and a retake as still indexing on
the change wait and as reader-unavailable on the connection wait, with the demand the read
recorded going back with the next hold of the gate.**

### Drivers

- **Least surprise.** A client that edits a note and then queries it sees the edit, even
  while other notes are being edited.
- **Trust over speed.** A source read never answers from a state it knows is stale.
  A retained count continuation identifies its original source separately from current trust.
- **No retry burden on every client.** Without the wait, every client, agents included,
  must catch a refusal and retry after each edit it makes.

### Known limit

This limit is amended: a reload leg opens the gap a maintenance leg opens. ADR 0049's
amendment stands: the background turn budget bounds a maintenance leg, semantic work opens
no gap, and a settling read may wait behind one maintenance leg.

While a maintenance leg holds the entry, the entry can stay `Ready` and not yet see an
edit made moments before, so a read in that window can miss that edit. A reload leg opens
the same window. It reads and judges its candidate outside the entry gate while it holds
the claim, so no poll runs, and an edit made after its fold's drain stays with the watcher
until the leg's end drain. A read that begins meanwhile answers at once: under `Ready`
where the fold found no facts and committed nothing, and under the healing the fold
published where it found some, since the facts the fold took are derived. This accepted
observation gap is limited to one maintenance leg or one reload leg. The maintenance leg
ends when the background turn budget runs out, plus the one unit in flight at that moment,
or earlier when its work is done. Watcher batches drain before another leg starts. The
reload leg's window lasts from the leg's start, at its fold's drain, to its end drain,
which takes in the edits that arrived meanwhile before the leg publishes, whether or not
the fold found facts. It covers a config-only activation's or a dry run's candidate read
and judgment. A schema-changing activation publishes warming once it has judged its
candidate and settles from then on, so it opens no such window after activation. The
candidate read is bounded by its own work, not by the background turn budget: reading and
parsing the vault's two control files, comparing the schema's fingerprint with the active
one, and, for a config-only activation, delivering the config to each engine receiver,
which opens nothing and leaves an engine's setup to the background lane. Its cost follows
those two files, never the vault's size or its edit rate. A semantic background turn and
the store's verification hold no claim and open no such window. The bound is not a promise
of wall-clock duration for an individual filesystem operation.

The reload leg's gap is accepted rather than closed because closing it would make reads
wait behind every reload leg for a drain, for a window of one candidate read.

The maintenance leg is what a settling read can now wait behind. A read waiting for the facts it
met may wait behind one maintenance leg, whether a reconcile leg handed the claim to it or
a deadline started it. The read waits for the rest of that leg: at most the rest of one
rolling-heal slice, which ends at its existing work bounds or the budget, whichever it
reaches first, one sweep step, and one death-prune unit, which is bounded to cost no more
than a slice. That wait is shorter than the
maintenance leg a read could wait behind under ADR 0048, which ran the whole verification
under the claim. The wait stays inside the read's bound.


### A schema reload serves reads again when its activation ends

Under ADR 0050, a schema-changing activation closed the reader, and the reload's own reconcile
turns kept the entry without one until `Ready` returned. Each turn's end drain that found
facts handed the claim to another such turn. So after a schema reload, a vault edited
continuously, by a sync tool for example, never served a read again. Every read the reload
settled refused at its bound as still indexing, and the reload's asker, answered only at
`Ready`, waited without a bound.

**The leg that activates installs the new pool as it ends, and the facts it leaves are
ordinary pending facts.** The activation derives the whole vault under the new declaration
inside its leg, so at that leg's end the store stands where a reconcile turn's end leaves it:
derived under the active declaration through every fact the entry has taken in, with the
facts its end drain delivered still pending. The leg installs the new pool there and ends as
every other leg ends: `Ready` where nothing is left, and otherwise healing with trust
unbroken, the claim going to the apply queue's head or to the reconcile owed. Installing the
pool over the store the entry kept breaks no trust: the store is the same one, its fact
positions run on, and its derivation under the active declaration is complete through every
fact taken in. Only a pool installed over a store that replaced the one before, as a rebuild
from zero or a fresh attach installs one, starts trust again. Ordinary reconcile turns derive
the facts left under the new declaration, and the entry settles across them as it does across
any change. A schema reload has no reconcile turns of its own.

**A read the reload settles waits for the edits the activation could not see.** No poll runs
while the leg holds the claim, so an edit made while the activation derives the vault stays
with the watcher until the leg's end drain. A read that met the change before the activation
closed the reader met only facts the activation derived, and it answers from the new pool as
the leg ends. A read that first meets the entry while the reader the activation closed is not
yet replaced records its position once the leg has taken in its end drain, so it waits for the
next intake, an ordinary reconcile turn or the apply queue's head, to derive that drain's
facts, never for `Ready`. A settling entry whose reader is missing for any other reason, such
as a failed mint, has no activation's end drain to wait for, and its reads keep the ordinary
rule. Without that rule such a read would answer
from the new pool without an edit made moments before it arrived, a gap as long as the
activation's derivation of the whole vault, where the known limit above bounds the reload
leg's gap by the work of reading two control files. With it, that limit is unchanged: a
schema-changing activation publishes warming once it has judged its candidate and settles
from then on. Installing the pool moves the signal a settling read waits on, so a read whose
facts are derived wakes when the pool stands, not at the next commit. A read that meets a
long activation still refuses at its bound, as it did under ADR 0050, since the activation's
derivation of the whole vault follows the vault's size.

The reload's asker is answered as the leg ends, as a config-only activation's asker already
is, so an answered schema reload no longer implies `Ready`; vault status reports where the
entry stands.

Considered options:

- **Bound only the asker's wait**, answering with where the entry stands once the settle bound
  passes. Every read the reload settles still refuses under a steady edit stream. Rejected.
- **Keep the reload's own turns under a bound** of turns or time, then install the pool. It
  keeps a second path for pending facts and adds a second bound to tune, to answer at `Ready`
  where no read needs it. Rejected.
- **Install the pool at the leg's end and let a read that met the activation answer from it
  at once.** It serves reads soonest, but opens an observation gap as long as the
  activation's derivation of the vault, bounded by the vault's size. Rejected.
- **Keep the old pool standing through the activation.** The re-pin clears every typed value
  and the admitting count lags the new ignore set until the activation's heal converges, so a
  read from the old pool would answer from a half-refilled store. Rejected.

## A from-zero build writes once, in steps

### Scope

A **from-zero build** constructs a canonical store from empty, and every one is built as
this section rules: a vault's first build, and every rebuild of an existing store from
zero — over a DDL fingerprint, derivation version or path order the store does not match,
over damage, or by any other rung that discards the database and derives the vault again.
It is the construction over which link health is judged once, as
[A from-zero build judges link health once](#a-from-zero-build-judges-link-health-once)
rules, under the same term. There is one construction path from empty to
complete; the ordinary per-changeset heal only maintains a store that already exists. A
full tree heal of an existing store, a recovery that does not rebuild from zero, a schema
reload, a park and a release gain nothing from this section and keep their existing read
rules.

A vault's first build and a rebuild serve reads alike: the same admission, under the same
warming trust label. Admission belongs to the claim of the leg that runs the build — the
attach leg, or the rung-3 leg — and to the watcher coverage that claim holds. A rebuild that
starts while a from-zero build runs abandons that construction and starts its own: the
capabilities the abandoned construction published are withdrawn, and the new construction
publishes its own under its own claim; it never remints the earlier authority.

**No reader can adopt a store under construction as complete.** The store records an
under-construction mark in `meta` when it is created, in the transaction that records its
DDL fingerprint, path order and derivation version, and clears the mark in the transaction
that ends step 5. The mark is a rebuild input beside those three
([ADR 0026](0026-a-derived-store-records-the-derivation-that-wrote-it.md)): an open that finds
it rebuilds from zero through the same ceremony a fingerprint mismatch uses, and reports why.
A crash or interruption before the transaction that ends step 5 commits therefore restarts
construction from zero and never resumes over what it left. A crash or interruption after
that commit leaves a finished store that may trail the vault — the reconcile before `Ready`
had not run — and the next open is the ordinary one: the attach heal converges it, as it
converges any store that trails the vault. A store whose recorded inputs match and which
carries no mark is a finished store, and its open is the ordinary one.

### The canonical store is written once, in five steps

A from-zero build writes each document's row once and fills it progressively. Nothing is
written to a store it does not stay in. The steps run in order over the vault, each in path
order:

1. **Names.** Every document row is written with only the names its path derives, and
   document ids are assigned in path order; every vault-file row is written final. A `.md`
   path whose bytes prove in step 2 not to be a document is quarantined. A step-2
   quarantine, or a file vanished before step 2, withdraws its provisional names row
   without a death record, as an ordinary build never wrote one. The gap in ids is accepted. A
   name a filesystem fact adds during construction is inserted through the ordinary add
   path and takes the next id, as in any store, so ids follow path order only for the names
   step 1 enumerated, and a build's ids equal an ordinary build's only where no `.md` was
   byte-quarantined and no name was added during the build. A
   directory the walk cannot list is withheld and marked, and the build continues
   ([ADR 0046](0046-one-unreadable-path-never-breaks-the-vault.md)). The name-resolution
   indexes are created at the end of this step.
2. **Content.** Each file is read once: its content hash, frontmatter projection, fields,
   and frontmatter link and tag facts are written, and its body is stored but not full-text
   indexed. A document the build cannot open is broken and skipped, as ADR 0046 rules: it
   keeps its names-only row, a finding beside the row names the cause, it holds no content
   or structure fact, and it has finished steps 2 and 3 as broken. The field indexes are
   created at the end of this step.
3. **Structure.** Each stored body is parsed. No file is read again: every fact comes from
   the bytes step 2 hashed. Headings, blocks, body links and inline tags are written. The
   indexes their readers seek are created at the end of this step.
4. **Relations.** Link health is judged once, over every eligible link, as
   [A from-zero build judges link health once](#a-from-zero-build-judges-link-health-once) rules.
5. **Full-text.** The full-text index is built from the stored bodies in bounded batches.
   The transaction that ends the step installs the full-text triggers and clears the
   under-construction mark.

**A step ends once the paths its closing take took have run through it.** Each requeue
takes the facts accepted so far once, and the requeue that closes a step makes that step's
closing take. No step starts while a taken path is still in an earlier one: step 4 never
ends with a taken path's links unjudged, and step 5 never starts while a taken path is
still in steps 2–3. A fact accepted after a step's closing take is taken by the next
step's requeue, which runs its path through every step the build has reached; a fact
accepted after step 4's closing take waits for the reconcile that precedes `Ready`, as
[A step ends at its closing take](#a-step-ends-at-its-closing-take) rules.

**Construction ends at step 5, and `Ready` follows in one order.** Step 5 installs the
triggers and runs the construction checkpoint; the facts accepted since step 4's closing
take are then reconciled as ordinary changesets, under the per-changeset rule of
[Link health is re-decided inside the changeset](#link-health-is-re-decided-inside-the-changeset)
and the restored autocheckpoint; and `Ready` is published.

Each step has one routine. The normal passes run it over the vault in batches; a requested
document, and a path a filesystem fact requeues, run the same routine on their own. There
is no second derivation path.

### Every document can be fetched and parsed on its own

Once step 1 has finished, a requested document runs steps 2 and 3 through their own
routines ahead of the queue and is answered as soon as its own step 3 is done. A Get by name
first runs every candidate the name resolves among, under the vault schema's
`paths.ambiguity_ignore` globs, through the step-2 routine, so its added work is bounded by
the candidate count, never by the vault. A request that arrives mid-build jumps the queue at
the next chunk boundary. Nothing waits for every document to finish a step before answering
one that is ready. Pending requests are bounded independently of vault size; a request past
the bound keeps the ordinary loading behavior.

Targets come from actual requests, including requests arriving during construction. No
target is configured before attachment merely to qualify for early service. Demand is owned
by the request's existing lease. Expired interest releases its place in the queue. Distinct
incoming requests cannot indefinitely delay construction. There is no vault-sized in-memory
target map.

### Capabilities switch on by data completeness

Each capability is admitted when every row it reads is complete, and not before:

- **Whole Get** when step 1 has finished and the requested document has finished its own
  step 3. Its body, fields, headings, blocks and links are exact. The resolution of the
  requested name itself is exact: every candidate it resolves among has been through step
  2, so an unknown or ambiguous answer is never given over a candidate that may yet prove
  not to be a document. Which document each of its links resolves to, and each link's
  health, read as still loading until step 4 ends: health findings exist only when step 4
  ends, and resolution is withheld with them so no Get shows a resolved link beside a
  loading verdict. Whether a path is a document is settled for every path when step 2
  ends; a document step 2 could not open stays a document. A Get of such a document
  answers unknown(unreadable) for each requested content column and carries the
  unreadable-document finding its own step 2 filed. Section and Block Gets answer the
  same unknown state, never a missing anchor or an empty body read as the document's.
  A document that breaks on a requeue after its own step 2 also becomes names-only,
  discarding every previously committed content and structure fact, as ADR 0046 rules.
  Its Get answers the same unknown state and finding. A projection of the document that
  reads other documents — its backlinks among them — is admitted when step 3 ends, and until then reads as still
  loading.
- **Field and path Find** when every document has finished step 2: predicates, projections
  and ordering over frontmatter fields and paths only, and only those its existing guarded
  builders support.
- **Tag predicates, and every Find shape that reads structure facts or other documents** —
  inline tags, links-to and backlinks, headings — when every document has finished step 3.
- **Link health, validate and `HasFinding`** when step 4 ends. Before then a `HasFinding`
  part is refused as not ready, and a findings column or page reads "still loading", never
  empty.
- **Full-text search** when step 5 ends.
- **`Ready`** after step 5, in the order above: every lane-1 capability. Writes and repair
  are admitted only then.

"Every document" counts the paths a filesystem fact has touched: a vault-wide capability
is admitted only once each of them has run again through the step it needs, whether a
later take or the reconcile before `Ready` runs it.

An early answer is exact or not given. No early row records a provisional link target or a
provisional absence. A request whose capability is not yet admitted, or whose shape no early
capability supports, keeps the ordinary loading behavior. A missing or unfinished step is
never a successful missing-document result.

Get and Find use one request-aware acquisition algorithm with canonical acquisition. The
ordinary public unrestricted read hold remains canonical. An early hold exposes only the
operation whose data is complete; it lends no unrestricted snapshot, reader, count,
rich-query, plan or mutation capability. Actual execution checks the request shape again
and invokes the ordinary builder. There is no second query language, filter, parser,
resolver, or client API.

A supported request that meets a from-zero build may wait for its capability within the
original read acquisition deadline. The first gate take, construction-availability wait,
capacity/open wait and gate retakes share that deadline. No phase renews it. Publication,
withdrawal and closure of a capability wake affected waiters; progress counters alone are
not an availability signal. No wait runs under the entry gate. Expiry while waiting for a
capability, the refusal of a capability not yet admitted and the refusal of a `HasFinding`
part before step 4 ends all name one code, `host/entry-not-ready`, with the current warming
phase and counters and the message "this vault is still being indexed".
Resource and gate-acquisition failures keep their existing refusal rules. This adds a
price: an eligible caller can be held up to the ordinary bound, whereas a canonical request
that meets a from-zero build still refuses at once.

### A filesystem fact withdraws only what it touches

A from-zero build is covered by one watcher authority captured before enumeration. A
relevant filesystem fact the authority accepts while the build runs withdraws only what it
touches, until its path is derived again:

- **Its path is derived again.** During steps 1–4 the next take requeues the path at the
  front of the queue, and it runs again through the same step routines, up to the step
  the build has reached; a path taken while step 4 runs has the links it reaches judged
  again before step 4 ends, as
  [A from-zero build judges link health once](#a-from-zero-build-judges-link-health-once) rules.
  After step 4's closing take nothing is requeued: the fact waits for the reconcile that
  precedes `Ready`.
- **A Get of that document waits** until its path is derived again. Other Gets are
  unaffected. After Relations, an unaffected Get answers with link resolution, link
  health and findings rendered loading while any fact is outstanding at its admission.
- **Every vault-wide capability waits for it** — field Find, the step-3 shapes, link health
  and search — until its path has run again, and for a fact accepted after step 4's
  closing take until the reconcile.
- **A create, a delete or a rename changes name rows at its take** during steps 1–4,
  through the ordinary add and remove path, and a Get whose name could resolve to an
  affected name checks its resolution again before it answers. After step 4's closing
  take, such a fact waits for the reconcile, and a Get whose name could resolve to an
  affected name waits with it. A create, delete or rename accepted after step 4's closing
  take therefore waits for the pre-`Ready` reconcile, because no take reaches it and name
  rows are not changed while the full-text triggers are absent.
- **Lost coverage withdraws every early capability.** An overflow, a lost root, a backend
  the watcher cannot cover, or any other loss of watcher fidelity withdraws every early
  capability, and neither draining a batch, finishing a heal window nor seeing a missing
  root return restores them. Construction continues, the store is reconciled with the
  vault before `Ready`, and requests meanwhile keep the ordinary loading behavior. A
  directory below the root whose watch cannot be installed because its own access is denied
  is not such a loss: the install withholds coverage at that directory alone and files no
  finding, and the walk that meets the same denial withholds and marks it, as ADR 0046
  rules. Its parent's watch reports a change to the directory's attributes, and that fact
  requeues the directory into construction as any fact requeues its path.
- **A requeued directory that lists again ends its finding before step 4 judges.** Its
  listing is a clean enumeration of that directory, so its unreadable-directory finding ends
  before any reader or step 4 can see the names beneath it, and step 4 judges over the restored
  names; where a take lists it while step 4 runs, the finding ends before step 4 ends and
  the links beneath it are judged again, as
  [A from-zero build judges link health once](#a-from-zero-build-judges-link-health-once) rules.
  After step 4's closing take the fact waits for the reconcile that precedes `Ready`, whose
  job-end prune ends the finding and re-decides those links.

The authority includes every relevant fact the subscription accepts after capture, wherever
it is held: the coalescer's pending or unplaced batch, an open heal window, collected heal
observations and the delivery slot are examples. The entry's merged `pending.taken_in`
counter alone is not this boundary. A request fixes its requirement when it meets the
entry; known facts cannot be omitted merely because the build has not taken them in yet. A
waiting request waits for the facts it met, never for the ones that arrive after it, rather
than moving its requirement until the filesystem falls quiet.

Publication of a capability binds the canonical store's identity, the capability, the
pinned content model, the watcher authority and the build's claim atomically under the
entry gate. A changed claim, registration, root, schema, known damage, park or release
prevents stale publication. Publishing a capability never sets `trust_unbroken_since_ready`
or global `Ready`.

A bounded root-identity check is permitted on this early acquisition path, outside the
entry gate and watcher locks. It reads no document and runs no derivation. The canonical
warm request path keeps its existing filesystem-check-free behavior. This is a scoped
exception to the architecture's request-path filesystem-check prohibition and the "never
per request" alternative below; it does not permit other filesystem query work.
Bookkeeping-only claim, authority, capability and deadline checks run under the
establishing gate hold; filesystem work, opening and waiting do not. Watcher callbacks never
take the entry gate, including when they withdraw a capability or wake a waiter. The lock
order is entry gate before watcher bookkeeping, never the reverse.

After a cold open or capacity wait, acquisition rechecks the current claim, capability,
authority, request shape and pinned model under the original deadline. Before exposing an
early result, the read checks that no accepted fact and no coverage loss known at that
instant withdrew the capability it answers under. Store, resource and environmental
failures keep their types even when eligibility also moves. Every source-dependent refusal
is an early result for this check: unknown or ambiguous target, cursor position/order
refusals and other judgments derived from indexed data cannot bypass it by being
represented as an error. Source-independent input errors retain their ordinary validation.
This is a watcher-fidelity guarantee, not filesystem atomicity or proof that an unseen
operating-system event did not occur.

### Full-text triggers are absent during construction

The full-text index is maintained by triggers on `documents`, and they are otherwise the
only writer of `documents_fts`. A from-zero build is a deliberate exception: the triggers do
not exist while it runs, step 5 builds the index from the stored bodies, and the
transaction that ends step 5 installs the triggers. From then on they are again the only
maintainer. No body changes while they are absent: a taken path rewrites its body only
during steps 1–4, step 5 starts only once every path taken before it has run through steps
2 and 3, and a fact accepted after step 4's closing take is never taken, so no body is
rewritten after step 5 has indexed any; every later body change
— the reconcile that precedes `Ready`, and the writes and repair admitted only at `Ready` —
is a changeset that runs after the triggers exist. A body changed after the index took it,
without the triggers, would leave the external-content index holding text the body no
longer holds and dropping text it never held. The finished store's schema equals an
ordinary store's.

### Construction keeps its cost following the work

- Each non-unique secondary index is created and committed at the end of the step after
  which its first reader is admitted, before that capability publishes, so an admitted
  statement's `EXPLAIN` gate sees the index; none is maintained row by row during the
  normal passes. Unique indexes and primary keys stay.
- An admitted early statement never scans a vault-sized table.
- Host chunks stay bounded at 128 documents. A SQLite transaction spans up to 2,048
  documents, and commits at the next chunk boundary whenever a request is pending, so the
  request's snapshot sees its document.
- The WAL autocheckpoint is raised during construction. The construction checkpoint runs
  when step 5 ends, before the reconcile; the restored autocheckpoint covers the reconcile.
  `synchronous` and the cache size keep their production values.

### Initializing a vault runs its first build in the foreground

Initializing a vault runs its from-zero build before it returns, so normal use has no cold
start; it returns once the vault is `Ready`. Early admission serves a vault used before its
initialization finishes. One file never fails a first index. A file whose bytes do not
decode as a document is quarantined as one document, under a finding naming it. A document
the build cannot open, such as one whose permission was denied, and a directory it cannot
list are marked broken and skipped
([ADR 0046](0046-one-unreadable-path-never-breaks-the-vault.md)): construction continues,
clears its mark and reaches `Ready`, and the vault is usable, with the findings naming each
such path. What ends a first index is a failure of the environment as a whole — a vault
schema that will not read, a store that will not open, a vault root that is gone or that
coverage cannot be installed over, the process's own exhausted descriptors or memory — or an
interruption. Either leaves the vault registered and initialization reports the
failure. Where it ends construction before step 5's transaction commits, the mark stands and
indexing restarts from zero on the vault's next use; after that commit, the next use opens
the finished store and its attach heal converges it.

### Promotion, budget and answers

Early reads run on read-only handles over the canonical store under construction, under the
same host-wide reader budget and fairness group as every canonical read. Get and Find
acquire source capacity, not retained-count scratch capacity. `Ready` closes early
admission and opens unrestricted canonical acquisition in the same gate hold that publishes
it. Healthy promotion and administrative retirement do not invalidate an already
established immutable read; its held proof and resources survive independently of new
admission. Explicit detach, unregister and host destruction count as administrative
retirement, including the unsubscription they cause. Established authority must
distinguish that administrative end from backend failure, overflow or root/source loss.
Dropping the subscription alone does not erase the proof a held read already earned or
permit new acquisition. An accepted fact or a coverage loss is different and withdraws the
early result it touches, including a source-dependent refusal. Teardown never waits for a
read or a cold open.

Damage met by an early read is canonical damage and takes the existing claim-bound path,
which discards the store and starts a new from-zero build; environmental failures do not
become damage.

An early answer carries the actual warming trust, store epoch and committed generation of
its establishing hold. Find continuations use ordinary cursor judgment: compatible epoch
movement continues with movement reported; incompatible order/schema retains the ordinary
refusal. No cursor is rewritten, silently restarted or implicitly promoted into a
retained-count result.

### What this changes and withdraws

**An early Get carries less than ADR 0043's did.** ADR 0043's early Get carried complete
findings and resolved links, because its closure source derived the candidates and linked
documents they need. An early Get here reads link resolution and link health as still
loading until step 4 ends, and a projection that reads other documents as still loading
until step 3 ends. Its body, fields, headings, blocks and links are exact, as before.

**A rebuild serves early reads, and a crash restarts it.** ADR 0043 admitted early reads
only during a fresh store's first attach, and a rebuild refused reads until `Ready`. Every
from-zero build now admits them. A crash during any from-zero build before the transaction
that ends step 5 commits leaves the under-construction mark, and the next open rebuilds
from zero.

**One fact no longer withdraws everything.** ADR 0043 withdrew every early capability on
any accepted fact. A fact now withdraws the document it touches and holds the vault-wide
capabilities until that document is derived again; only a loss of coverage withdraws every
early capability.

ADR 0043's disposable sources have no subject here and are retired with every rule that
existed only for them: the disk-backed names catalogue, the sealed metadata intake store,
the closure Get sources and their name and link closure bounds, the per-attach budget of
target preparations, the loading directory beside the canonical store and its sweeps, the
rule that a source's writers close before its readers open, the separate identity, epoch
and damage judgment of a disposable source, the rule that physical temporary-file names buy
no extra fair turns, cleanup receipts for temporary storage, and the promotion that retired
published sources at `Ready`. A Get answered early no longer needs a rich projection derived
apart from the store: the store's own rows are the projection.

**The architecture's contract follows.** Rung 3's triggers gain the under-construction
mark beside the fingerprint, derivation and path-order mismatches. The failure table's
process-killed row gains a from-zero-build clause: a process killed during construction
before step 5's transaction commits leaves the mark and is rebuilt from zero, link health is
judged once rather than re-decided per changeset during that build
([A from-zero build judges link health once](#a-from-zero-build-judges-link-health-once)),
and a process killed after that commit leaves a finished store the attach heal converges.
The statement that the full-text triggers are the only writer of the full-text index gains
the construction exception above. The permission-loss row changes as
[ADR 0046](0046-one-unreadable-path-never-breaks-the-vault.md) rules: an unopenable document
or unlistable directory no longer refuses the heal or fails a first index.

## A step ends at its closing take

### Context

ADR 0045 ruled that a step "ends only once every path requeued during it has run through
it". The build carried that out by taking the open heal window again and again until a
take came back empty: each take derives its changed paths again, commits, and runs a
scoped walk. A heal window gathers facts with no debounce, since the quiet window and
batch-age bound that shape ordinary reconcile batches do not apply to it. A file rewritten
faster than one take completes therefore leaves a fact behind every take, and the step
never ends. One take cost about 2.5 ms on a developer machine. A writer rewriting one note
at 1 kHz stretched an attach to 10–18 s; with 20 ms injected per take, a writer at 60–100 Hz
kept the attach from ever finishing. A sync tool or plugin that rewrites one note a few
dozen times a second on a slow machine or a large vault reaches that regime, and its vault
never reaches `Ready`. The same rule let a continuous-writer test hold the shared
real-watcher test lease for minutes.

### Decision

**Each requeue takes once.** A requeue takes every root accepted up to that instant, the
same take as before, derives those paths again through every step the build has reached,
commits and publishes their derivation, and returns. It does not take again. Names calls
requeue once, as it closes. Content and Structure call it as the first group of their
normal pass opens, at every chunk boundary inside a group, as each group closes, and once
more as the step closes; only that last call is the step's **closing take**. Relations calls it once, after
its link-health judgment, and that call is its closing take. A step ends once the paths
its closing take took have run through the step.

A fact accepted during a step waits for that step's next requeue. In Content and Structure
that is usually the next chunk boundary. Relations has only its closing take, so a fact
accepted early in Relations waits for the whole link-health judgment. A fact accepted
after a step's closing take goes to the next step's first requeue, which runs its path
through the earlier steps again on its way to the current one. A fact accepted after
step 4's closing take, which closes Relations, is never taken by the build: it waits for
the reconcile that precedes `Ready`, the same path every fact accepted from step 5 on
already took. The reconcile receives only untaken facts, as before.

A path that no take has derived again stays outstanding. A Get the path touches and every
vault-wide early capability wait for it, as a request waits for the facts it met; none is
answered from the build's earlier derivation of that path. A fact accepted during steps
1–4 is waited on until the next take; one accepted after step 4's closing take is waited on
until `Ready`.

### Consequences

- **Construction ends under continuous edits.** Construction no longer waits for edits to
  pause: it ends and hands on to the reconcile before `Ready`. That reconcile publishes `Ready`
  once a turn leaves no fact pending, so under edits that never pause the vault stays
  `Warming` until they do, as any attached vault does. A build's requeue work is bounded by its
  requeue calls, not by the number or rate of facts accepted while it runs. The calls
  follow the vault's shape: a build under edits makes no more of them than a quiet build
  of the same tree, and can make fewer, because a document pulled ahead leaves its normal
  pass with fewer chunks to page. On the 200-document test vault the build made about 210
  requeue calls and never more takes than that. With a writer rewriting one note as fast
  as it could, which produced about 50,000 accepted facts during the build, the attach
  took about 0.7 s, as it did with a writer near 1 kHz. With 20 ms injected per take, it
  took about 5 s with a writer at 100 Hz and with an unthrottled writer, whose roughly
  440,000 facts the build accepted; under ADR 0045 those attaches never finished. The bound is on work, not on
  wall time: host load still stretches the attach. With 24 busy loops on 12 CPUs, the
  test with a writer near 1 kHz took about 5 s against about 0.7 s on a quiet host, and
  still finished.
- **One edit after Relations' closing take withholds link health, Find and search until
  `Ready`.** Any single fact accepted after step 4's closing take stays outstanding until
  `Ready`, so a path edited even once in that window withholds them, not only a path
  under continuous edits.
  A Get of that path waits for `Ready` instead of answering early, and so does every
  vault-wide early capability that meets such a fact: link health, validate and
  `HasFinding`, every Find shape, and search. A Get the outstanding facts do not touch
  keeps answering, with link resolution, link health and findings rendered loading while
  any fact is outstanding at its admission, as before.
- **The pre-`Ready` reconcile can carry more.** Facts accepted between step 4's closing take
  and step 5 now reach the reconcile rather than a further take. Its cost follows those
  facts' reach, as it already did for facts accepted from step 5 on.
- **An invariant pins the rule.** A take is one call of the build's take that returns
  a non-empty set of roots. The number of takes never exceeds the number of requeue
  calls, whatever the event rate. A test counts takes at their own construction boundary
  and calls where each caller marks one just before it calls, outside the requeue, so no
  change to how a requeue takes can change the call count; both counts stand apart from
  the counters the build keeps. It builds the vault once with no
  writes, which makes 24 requeue calls and no take. It then builds it again under a
  writer that keeps a fact pending at every call. The writer rewrites one note twenty
  times inside every take, before the take reads it, and once after each take's read. It
  rewrites the note again once Structure publishes, and rewrites a second note once
  Relations publishes. Each rewrite waits to be accepted. That build makes 22 requeue
  calls, no more than the quiet build, and exactly one take per call, while it accepts at
  least ten facts per call. Its counters agree with the boundary counts, and the store it
  reaches `Ready` with equals a from-scratch build of the final bytes, so a fact a take
  left behind was not lost. Under ADR 0045's rule an earlier form of this test, writing
  only inside takes, took again within one requeue until its writer stopped at a 400-take
  cap: 401 takes against 24 requeue calls. A
  second test attaches a 201-document vault under a writer near 1 kHz. It holds the
  build's requeue calls to a quiet attach of the same tree, which makes 212, and its
  takes to its calls. It then waits for the store to hold the writer's last bytes.

## Whole-vault and semantic readiness

This section is amended. Its scheduling rules, its install rule and its rule about
publication are replaced by the section
[Background work runs on its own lane](#background-work-runs-on-its-own-lane). Its other
rules stand as ADR 0048 wrote them.

Global lane-1 `Ready` still requires a from-zero build through step 5, watcher
synchronization and reconciliation of the accumulated facts, in the order construction
states. Request-specific early permission does not weaken that boundary. Canonical
lexical/rich queries become useful at that boundary even while optional semantic
initialization or feed consumption remains outstanding. Semantic engines start after
`Ready`: no sidecar opens and no model initializes while a from-zero build runs.

Configuration interpretation and delivery still precede publication of the canonical
declaration. Delivery validates the section and records its desired configuration identity
and actual availability; it no longer eagerly opens a sidecar or initializes a model.
Where initialization is owed, the delivered reading says it is pending, rather than
claiming an enabled runnable engine, a disabled engine, or an initialization failure.
An already running compatible engine may retain its actual availability without reopening.
The publishing gate hold commits the declaration fingerprints, delivered section and that
truthful availability reading together. This replaces the architecture's eager-enable
meaning of config delivery while preserving declaration/delivery coupling.

Initialization includes sidecar open or creation, inspection/rebuild and model setup;
it is not another name for consuming the first feed page. Those operations follow the
canonical publication that committed their configuration. Until an engine is usable,
semantic requests follow the existing unavailable-rung behavior and status distinguishes
pending initialization from disabled or failed configuration. Initialization success or
failure is published only while its delivery and its store lifetime are current, as
[A result installs only while its delivery and store lifetime are current](#a-result-installs-only-while-its-delivery-and-store-lifetime-are-current)
rules. Disable, reload, replacement and teardown retire obsolete pending
work; a late result cannot install an engine for an older declaration or retired store.
Its resources close outside the entry gate. No model or sidecar I/O runs under that gate.

Semantic initialization, store-epoch reconciliation and feed consumption run as
background units on the background lane, under the rules of
[Background work runs on its own lane](#background-work-runs-on-its-own-lane).
Vault-sized sidecar reconciliation is not exempt from those rules: it runs one reconcile
page at a time. Config, sidecar ownership and feed epoch and generation keep their lane-2
contracts, apart from the explicit delivery and initialization split. Cancellation and
teardown keep theirs as that section's retirement rules amend them. Status and search
answers keep the engine's actual availability and watermarks. Semantic work neither
writes correctness state nor blocks ordinary lexical readiness merely because it has more
feed to consume. A model-free host makes no semantic availability claim, and scheduling
tests do not establish real-model cost or quality.

An attachment that recreates its semantic engine can publish canonical `Ready` before
the vector rung becomes usable, including a warm reattachment after idle detach.
Status reports preparation during that interval; its duration is not promised here.

## Background work runs on its own lane

### Context

Background work today runs as a maintenance job on the shared worker pool, under the
vault's claim, holding the vault's attachment. Three costs follow from that placement.

- **A semantic slice holds the store writer.** The feed-read handle borrows the
  attachment's `Store` exclusively, so a drain page runs under the claim, and every write
  to that vault waits behind it. One page holds up to `DRAIN_PAGE`, 256, feed rows. With
  the stub embedder a page is estimated to cost microseconds. With a real model a page of
  up to 256 embeddings is estimated to cost seconds, and each of them is time the vault
  cannot take an apply.
- **Semantic work never converges under continuous edits.** A slice runs only on a quiet
  leg: over a `Ready` entry that owes no heal rung, has no queued apply and no pending
  watcher facts, and has no teardown due. A vault that someone keeps editing is never
  quiet, so its embeddings fall further behind for as long as the edits continue.
- **Verification holds the attachment for a full scan.** `Store::verify_integrity` runs
  `PRAGMA integrity_check`, which reads every page of the database, and recomputes every
  document's body sub-fingerprints. While it runs, the entry's coverage is out with the
  leg, so no watcher batch drains and every write waits. The scan's cost grows with the
  database, once an hour per attachment.

Start-up has a cost too. Work becomes owed at a commit. A reconcile leg and a maintenance
leg each ask at their end whether maintenance is due, and schedule the maintenance leg
there when it is. Work that a leg with no such check leaves owed, such as an apply that
hands on no reconcile, starts only at the next dispatcher poll tick whose watcher poll
finds it due. Its latency then follows the tick interval rather than the work.

The work splits by whether it needs the vault's claim. Semantic work and the store's
verification need none, and they are the work whose cost can reach seconds: the
verification's one write, the full-text check, can run on a copy of the store rather than
on the store. The rolling heal and the shadow sweep need the claim, and each runs in
bounded steps. This decision moves the first kind to its own threads and keeps the second
on the worker pool under a time bound.

### The lane serves derived stores in background turns

The background lane is a set of threads in `norn-host`, sized by the lifecycle policy
value `LifecyclePolicy.background_threads`. It sits beside `worker_slots`, which sizes
the shared worker pool, because the lifecycle owns that pool and the dispatcher, and so it
is where the lane's scheduling lives. `LifecyclePolicy` has no library default: the
composition root sets every value, and it sets `background_threads` to 1 in production.

The lane runs **background units**, the background work that needs no claim: an engine's
initialization, its feed and reconcile pages, and the store's verification. The
lane holds the one host-wide background queue: a coalescing set of owed derived stores. A
derived store, not a vault name or a root, is the key, as it is for the read budget,
because two registrations over one root have two stores under
[ADR 0016](0016-maintainer-singleton-per-derived-store.md). The host-wide background
budget is the lane's thread count. No background unit runs anywhere else, and no
worker-pool job carries one.

The background lane is not a derivation lane. It runs lane-2 work (engine consumption)
and lane-1 work (the verification of the lane-1 store) alike. The lane adds no
crate and no dependency edge.

A thread takes one derived store from the set and runs one **background turn** for it:
units of that store's owed work until the background turn budget runs out or nothing is
owed. A store that still owes work when its background turn ends goes to the back of the
set. One store with a large backlog therefore takes one background turn while every other
owed store takes one, and it cannot starve them. At most one background turn runs for a
derived store at a time. A store whose unit its environment refused, or whose background
turn unwound, still owes its work with no wake coming, so the lane asks the background
scheduler to ask for that store again after a short fixed backoff.

The shared worker pool keeps correctness work (reconciles, applies and heals), readiness
work (attach, the from-zero build, recovery, rebuild, detach and reload) and the
maintenance legs that run the claim-bound slices. The per-entry claim keeps ordering that
work exactly as it does now. The dispatcher keeps its tick for the watcher polls, the idle
reap, the retry of refused dispatches and the trimming of idle readers. None of those
starts background work.

### A background turn is bounded by time, with one unit as its floor

`LifecyclePolicy.background_turn_budget` is the time one background turn may take. The
composition root sets it to a value chosen by measurement, and the implementation records
the measurement beside that value. The expected range is 100 ms to 250 ms. A background turn checks the budget only
between units, and it always runs at least one unit. It therefore ends within its budget
plus one unit. The lane counts the units each background turn runs beside the turn's
clock, so a budget too small for any unit shows as exactly one unit per background turn.

A unit is the smallest step that is not interrupted:

- for an engine, its initialization, or one feed page or one reconcile page of its drain,
  with the census pages read beside that page (see
  [Status reports embedding coverage](#status-reports-embedding-coverage));
- for the store's verification, the whole verification: its read-only checks, its copy
  of the store and the full-text check on that copy.

One embedding is never interrupted, and neither is one page or the census pages read
beside it. The engine's page size, with the census's page of each feed, is therefore what
bounds the overrun of a semantic background turn.

The verification is the one unit not at human scale, because it reads every page of the
database and writes a copy of it. Host shutdown interrupts it, and so does the retirement
of the store lifetime it reads. Either one sets the store lifetime's retirement mark first,
and then calls `sqlite3_interrupt` on the connection the verification is running a
statement on: a read-only check's, the copy's or the copy's full-text check's. The
verification is several statements on three connections, and `sqlite3_interrupt` stops
only a statement in flight, so the mark covers the rest:

- The verification reads the mark before each statement, between the rows it reads where
  it recomputes body sub-fingerprints, before its copy and before the copy's check. A set
  mark ends the verification there.
- The interrupt stops the statement in flight when the mark is set.
- A statement that starts between the verification's last read of the mark and the
  interrupt would escape both. Each of the verification's connections therefore carries a
  progress handler that reads the mark, and a statement stops within one handler interval
  of the mark being set.

An interrupted verification reaches no verdict, and its copy is removed. It returns within
one handler interval, or one document's recompute, after the mark is set.

A work bound alone does not fit a lane whose unit cost depends on the embedder. A bound
sized for the stub gives background turns estimated at seconds under a real model. A
bound sized for a real model makes a stub background turn mostly scheduling overhead. A time bound
follows the actual cost, and the floor of one unit means every background turn makes
progress.

### Work starts at a wake or a deadline, with no poll backstop

This section is amended. The death prune's clock is a third claim-bound deadline. Its other
rules stand as ADR 0051 wrote them.

The **background scheduler** is one thread in `norn-host`. It keeps every background
deadline, lane and claim-bound alike, and it receives every wake. It does no background
work itself. It sleeps until a wake arrives or the earliest deadline comes, and then
reads the owed flags and due clocks of each derived store:

- Where a store owes lane work, the scheduler puts the store in the lane's set and wakes a
  lane thread.
- Where a claim-bound slice is due, the scheduler asks for a maintenance leg for the
  store's entry, as the paragraph below describes.

The deadlines are:

- for the lane, a verification due, hourly for each store;
- for a maintenance leg, a rolling heal due or a rolling heal pass still in progress, a
  shadow sweep's age threshold reached, and a death-prune pass due or still in progress, as
  [When a death prune runs](#when-a-death-prune-runs) rules.

A lane deadline that has come is owed lane work rather than a deadline, so the ask that
finds it puts the store in the lane's set and contributes no deadline for it. The unit
that runs moves the clock on, and the lane thread hands the store's next lane deadline back
to the scheduler as the background turn ends, so an idle store comes due again with no
wake.

**The scheduler asks for a maintenance leg through the existing scheduling path.** It
takes the entry's gate, as the dispatcher's retry does, and reads the entry's background
clocks there: the entry keeps a reading of its attachment's earliest background deadline,
because the attachment is out with a leg exactly when the claim is held. The reading is
refreshed where the attachment is installed and at every ask with the coverage in hand; a
leg that parks the coverage back may leave it as it stood. It can only be early, never
late, because a leg moves a clock only by running the unit that clock times, which sets it
later: an implementation of the clocks owes that a leg only ever moves one later. The
release that leaves an entry unattached ends its background clocks, whether a detach, an
unwound leg's discard, or the give-back after a refusal or a loss gave the coverage back,
so an unattached entry contributes no deadline, and the next attach starts them again. The
scheduler schedules only where the entry's coverage is in hand and no release is in
flight, as the poll does today. Where a claim-bound slice is due, the claim is free, no
teardown is due and the entry owes no heal rung, it schedules a maintenance leg under the
claim's scheduled marker and sends it to the worker pool, as a poll schedules one today.
Where a full queue refuses the send, the marker stands with the claim held for it, and the
dispatcher's retry of refused dispatches sends it. That retry sends a leg that was already
scheduled. It never decides that maintenance is owed. **At every ask where a claim-bound
slice is due, the scheduler either schedules the leg or, where any condition above stops
it (the claim is held, the coverage is not in hand, a release is in flight, a teardown is
due or a heal rung is owed), marks the entry as owing maintenance. The mark stands through
the scheduling and through a leg's hand-on to a maintenance leg, so the claim's release
after that leg wakes the scheduler to read the clocks again; it clears the mark only at an
ask where no claim-bound slice is due**. A scheduled or marked entry contributes no
claim-bound deadline, so a rolling heal past its due time cannot wake the scheduler again
and again for the length of a reconcile, and an entry whose mark is cleared contributes
its next deadline again, so an idle vault keeps its hourly heal and its sweep. The release of the claim on a marked entry wakes the scheduler, which asks again;
the ask on a marked entry runs whether or not a slice is still due, so a leg that reset
the clock clears the mark. That wake is sent by the claim carrier's own moves that open
the gate (a release, an open, the end of a poll, the drop of a marker), so no release site
outside the carrier can omit it. The entry gate is taken before the scheduler's lock. The
scheduler's lock and the lock on the lane's set are leaves: no thread holds either across
a take of an entry gate or across a unit.

**Leg ends keep asking `maintenance_due`, as today.** A reconcile leg that ends with
maintenance due hands the claim on to a maintenance leg ahead of another reconcile turn,
behind a queued apply and a due teardown. A maintenance leg that ends with maintenance
still due schedules another, behind the reconcile its observed facts owe. The watcher poll
stops scheduling maintenance. A poll that carries facts schedules their reconcile, and
that reconcile's end hands on to due maintenance. The poll therefore no longer decides
maintenance, and under sustained watcher traffic the fairness of maintenance holds.

A wake is contentless. It says that something may be owed, and the owed flags and due
clocks say what. These sites send one:

- a commit that leaves an engine owing work;
- an engine that becomes ready to initialize, which is the publication that commits its
  delivery;
- a publication that returns an entry's read stance from refuse to serve or settle, such
  as a recovery that did not rebuild, so the engine of an idle vault resumes feeding
  without waiting for a commit (a move that also brings a new store costs one extra ask);
- an attach, which starts the entry's clocks;
- the release of the claim on an entry marked as owing maintenance.

No poll tick starts background work, on the lane or in a maintenance leg. The wake is
host-internal and never travels on the filesystem event bus (invariant 9). A wake sets a
flag under the scheduler's lock, so a wake sent while the scheduler is busy is kept. The
owed flags and due clocks stay authoritative, so a lost wake costs latency and never
correctness, as [Lane 2 reads change through a feed](#lane-2-reads-change-through-a-feed)
rules for the feed. A wake is lost only where a site that owes one does not send it. The
latency then lasts until the next wake from any site, the next deadline, or, for a
maintenance leg, the entry's next leg end that asks `maintenance_due`. A marked entry has
no deadline, so its latency has no time bound of its own.

A detached vault gets no background work until its next attach. Outstanding background
work holds no demand and never defers an idle detach. ADR 0048 ruled this for semantic
work alone. This decision makes it the rule for every background unit and every
maintenance leg. Remaining work resumes after a later attachment, under the then-current
configuration and store.

### Semantic work leaves the claim

This section is amended. A drain also reconciles where the engine has not read through the
death floor, and every reconcile records the write generation its first page read and, on
completing, that generation itself as read through. Its other rules stand as ADR 0051 wrote
them.

A semantic background turn reads the change feed through its own read connection, never
through the attachment's `Store`. It embeds while holding no store and no lock, and it
commits to its own sidecar. It takes no claim and no attachment, so it runs alongside that
vault's reconciles and applies. They share only CPU, which the lane's thread count caps.
Embeddings therefore converge while edits continue.

Correctness rests on mechanisms that already exist, and on the death floor. Cursors are
consumer-owned feed positions, each a `generation:path` pair, valid within one store epoch
and only while the engine has read through the death floor. The sidecar records the store
epoch it observed beside its cursors (`OBSERVED_STORE_EPOCH`), and beside that epoch the
generation its last completed reconcile read through, the write generation that reconcile's
first page read. A feed whose epoch differs from that record, or whose floor the engine has
not read through, makes the drain reconcile the sidecar against the store before it reads
the feed: the rescan
[Lane 2 reads change through a feed](#lane-2-reads-change-through-a-feed) rules for either
failure. A reconcile records the write generation its first page reads, never starts over
because of the floor, and reconciles again where it completes with the floor above that
generation, as [The death floor](#the-death-floor) rules. The drain's generation check already keeps a hash from being written beside values
it does not describe, even against a writer interleaving through another store handle. The feed-read
handle keeps its surface. Only its source changes, so `norn-semantic` still reads lane-1
state through that handle alone (invariant 12).

**A semantic unit embeds outside the slot lock and touches the sidecar only under it.** A
feed page runs in three phases:

1. Under the engine slot's lock, the unit checks that its delivery and store lifetime are
   current. It reads the feed page, compares each row with the sidecar, and fetches the
   stored facts of the rows it must embed, all in one WAL snapshot transaction. It then
   releases the lock.
2. Holding no lock, it embeds those rows.
3. Under the slot lock again, it checks currency once more and commits the page and its
   cursor to the sidecar. A unit that finds its delivery or its store lifetime retired
   discards its page and commits nothing.

The currency check reads only the slot's own retirement mark, under the slot lock. It never
takes the engine map's lock. The existing lock order is the entry gate, then the map lock,
then the slot lock. A unit takes neither the entry gate nor the map lock while it holds the
slot lock, so it cannot invert that order.

An initialization runs its model setup holding no lock. It opens, creates, inspects or
rebuilds the sidecar under the slot lock, after the same currency check. A reconcile page
and a tombstone page embed nothing, so each runs whole under the slot lock.

**The feed connection belongs to the engine's delivery.** `norn-store` mints the feed
source from the live `Store` in the gate hold that commits the delivery beside a store
lifetime, the same hold that records the delivery's published store today. Minting opens
no database and runs no SQL, as a reader pool's mint does. The connection opens lazily on
the lane, read-only, and an establishment against another store epoch is refused. A unit
uses it only under the slot lock. Each page is one WAL snapshot transaction, which never
blocks the writer. The connection is outside the request read budget, beside the separate
bounds that [Elastic read capacity](#elastic-read-capacity) keeps for source writers,
private apply readers and sidecars. There is at most one feed connection for each
delivered engine. It closes when its delivery or its store lifetime retires.

Two other sources were weighed:

- **The request read budget.** A background turn holding a budget connection makes a
  read wait for capacity, which is the delay this decision removes. It would also put
  background turns into the read fairness queue.
- **A fresh connection for each background turn.** This is correct, but it pays a cold
  open and a cold page cache on every background turn. Under continuous edits that cost
  multiplies with the number of background turns. A delivery-owned connection costs one
  connection for each enabled engine, the same order as the sidecar connection the engine
  already holds.

**A background turn feeds only while the entry's read stance is serve or settle.** No
sidecar opens and no unit feeds an engine while its entry's stance is refuse. That covers
a from-zero build, a rebuild included, and a recovery that does not rebuild from zero. A
reconcile's settle stance means the vault is taking in a change, not that its store is
unfit to feed from, so background turns keep feeding through it. A published damage
verdict moves the stance to refuse. Background turns then pause until the rebuild
publishes a new store lifetime, so no background turn feeds from a store whose damage the
entry has published. The rebuilt store has a new epoch, so the engine's next drain
reconciles the sidecar against it.

**Damage a feed read meets is judged as damage a request read meets.** It enters the damage
judgment that [Source reader acquisition](#source-reader-acquisition) gives damage a read
meets, and that judgment schedules the existing rebuild as correctness work through the
claim, never a heal inside the unit. The verdict counts only for the store lifetime the
unit read, so damage met over a lifetime that has since retired schedules nothing. The
unit commits nothing from that page. An environmental failure, such as an open failure, a
busy database or descriptor exhaustion, is not a verdict. It schedules no rebuild, and the
engine stays owed.

**A background turn never delays a leg's publication.** It holds nothing a publication
takes. ADR 0048's rule that no semantic slice runs between a lane-1 commit and the
publication of its `derived_through` or `Ready` is withdrawn, because that rule guarded a
slice that held the claim. Every leg that commits lane-1 work still records the store
reading and the owed flag before its publication, so a status taken before the background
turn runs reports the engine trailing.

**A search waits behind at most one unit.** The vector rung takes the engine slot's lock.
A unit holds that lock for its read phase, its commit, a sidecar open or one reconcile
page, never across an embedding or a model setup, and never for a whole background turn.
The slot lock is a `std::sync::Mutex`, which promises no order among its waiters, so a
lane thread that releases it could take it back ahead of a waiting search. The
implementation therefore makes the lane yield the slot to the searches already waiting
when the lane would take the lock, and never to a search that arrives later. For example,
each search takes a ticket from a counter before it takes the lock, and marks its ticket
served once it holds the lock, or returns or unwinds before reaching it. A lane thread
reads the last ticket issued and leaves the lock until every search up to that ticket is
served. The same mutex lets a search that arrives later take the lock ahead of a lane
thread waiting for it, and a stream of such searches would starve the lane, so a search
whose ticket was issued after the lane came to the lock waits, before it reaches the lock,
until the lane holds it. A vector rung therefore waits behind at most one hold of the slot
lock, and answers between units or between the phases of one. Continuous search delays the
lane only by the searches that were waiting when the lane came to the lock.

### A result installs only while its delivery and store lifetime are current

This replaces ADR 0048's rule that an initialization installs only for the claim it began
under. A background turn holds no claim, so the claim cannot be the test.

An initialization installs its engine only where the engine map still holds the very
slot it opened for, and that slot's published store lifetime is the one the
initialization read. The slot's identity is its delivery's configuration identity. A
disable, a reload that replaces the section, a detach followed by a re-attach, and a store
replacement each leave another slot, another store lifetime, or no slot, so each refuses a
late result. Resources a refused result holds close after the check, outside the entry
gate and the engine locks.

**Retirement takes the slot lock, and only then gives back what it guards.** A detach, a
disable, a replacing delivery, a store-lifetime retirement and host shutdown each retire a
delivery in the same order:

1. Under the slot lock, mark the delivery or its store lifetime retired, and take out the
   engine and the feed connection.
2. Release the slot lock, then close the sidecar connection and the feed connection.
3. Where the store lifetime retires, interrupt a verification running over it, and wait
   for the verification to return. Its copy is removed before it returns.
4. Only then go on: a detach releases the store writer and the maintainer lock, and a
   rebuild discards the store.

Under [ADR 0016](0016-maintainer-singleton-per-derived-store.md) the maintainer is
singular for each derived store, and every mechanism a maintainership owns is keyed like
its lock. The sidecar lives in the derived directory the maintainer lock protects. A second host
can discard and rebuild the same sidecar once the maintainer lock is free. Every sidecar
commit is made under the slot lock after a currency check, and retirement takes that lock
before the maintainer lock goes back. No commit therefore reaches a sidecar after this
host's maintainership of it has ended.

**A rebuild retires the store lifetime at its discard, not at its publication.** The
retirement runs before `discard_and_reopen` removes the database, not when the publication
that records the rebuilt store's lifetime commits the delivery beside it. No background
turn therefore reads a store that has been removed: a feed read runs only under the slot
lock, and retirement closes the feed connection before the removal.

**The bounds of a retirement are these.**

- A detach, a disable, a replacement, and a rebuild's discard each wait for at most one
  slot-lock hold of a unit in flight. A detach and a rebuild's discard retire the store
  lifetime, so each waits besides for the return of an interrupted verification: one
  progress-handler interval or one document's recompute. None of them waits for a model
  setup or an embedding.
- Host shutdown starts no new unit, retires every delivery as above, and waits for each
  lane thread to end the unit it has in flight: at most one unit for each thread. A
  verification in flight is interrupted. A semantic unit in flight finishes its embedding
  and then discards its page. A maintenance leg in flight ends as a worker-pool leg ends
  at shutdown today.

A background turn checks currency before each unit and stops at the first unit boundary
after its delivery or store lifetime retires. A report the background turn writes into a
retired slot reaches no reader of a current delivery. A later initialization over the same
sidecar is a unit of the same derived store, so it starts only after the retired
background turn's unit in flight ends.

**An unwind out of a unit is caught at the lane's seam,** and the lane thread outlives
it. The engine slot reads the unwind as an unwinding drain does today. An unwind during
the embedding phase leaves the engine in its slot and commits nothing. An engine borrowed
under the slot lock stays in its slot with the feed connection the unit had out, and the
unit's work stays owed. An engine taken out for its sidecar rebuild is abandoned and reads
as self-disabled. An unwind out of a maintenance leg goes through the
cleanup that reconciles an unwound worker-pool leg today.

### The store's whole verification runs on the lane, its full-text check on a copy

`Store::verify_integrity` runs read-only checks and one write, the full-text check. The
periodic verification runs all of them as one background unit through read-only
connections to the store, and runs the write on a copy of the store. Nothing in it holds the claim, the
attachment or the store's writer. Every check keeps its classification of a failure: only
a driver error that describes the file's own contents is reported as damage, and every
other failure is reported as the refused operation it was. A verdict of damage is what
authorizes discarding the database, so the classification decides which failures schedule
a rebuild.

**The read-only checks run first, over one snapshot.** `PRAGMA integrity_check`, the
foreign-key check, the `json_valid` scan of the frontmatter projection, the closed
vocabularies, the rule rows and rule sets, the pillar and quarantine checks and the body
sub-fingerprint recomputes read through a read-only connection of the verification's own,
over one WAL snapshot. `norn-store` mints the verification's source from the live store,
as it mints the feed source, with no I/O. Its connections open for the verification and sit
outside the request read budget. Each closes when its step ends: the snapshot connection
before the copy is written, the copying connection once the copy is written, and the
copy's connection when its check ends.

**The full-text check runs on a copy.** `INSERT INTO documents_fts(documents_fts, rank)
VALUES ('integrity-check', 1)` is a write, and it takes the write lock on any connection
that runs it. Run on the store, it held the writer for about 104 ms at 10,000 documents,
which is a hold at human scale. The verification therefore copies the store and runs the
check on the copy:

1. A second read-only connection runs `VACUUM INTO` a scratch file. The copy is the store
   as one snapshot reads it, so writing it never holds the store's writer. Its connection
   admits that one statement into that one file, and nothing else a sealed read connection
   refuses.
2. The full-text check runs on the copy, through a connection to the copy alone. It takes
   the copy's write lock, which nothing else wants.

A verdict about the copy's index is a verdict about the store lifetime the copy was taken
from. `VACUUM INTO` rewrites every page, but it keeps every rowid the full-text check
compares: `documents` keys its rowid as `id`, which the index names as its content rowid,
and each of the index's shadow tables either keys its rows explicitly or is `WITHOUT
ROWID`. The copy's index therefore disagrees with the copy's documents exactly where the
store's index disagrees with the store's documents. Damage to the pages themselves does not
survive the rewrite, and `PRAGMA integrity_check` catches it on the store's own snapshot. A
`VACUUM INTO` that fails on the store's own contents is damage. A failure of the
scratch write itself, such as a full disk or a directory that refuses the file, is a
refused operation and never damage. The copy reads a later snapshot than the read-only
checks, which is no gap: each verdict is about the lifetime both snapshots read.

**The copy lives beside the store.** It is a file in the vault's derived directory, on the
store's own machine and filesystem and never in the vault. It is removed on every way out
of the verification: a verdict, a refusal, an interruption and an unwind. A copy that a
crash left behind is removed at the next attach, and before the next copy is written. The
retirement of the derived state removes it with the store.

A damage verdict enters the damage judgment that
[Source reader acquisition](#source-reader-acquisition) gives damage a read meets. That
judgment schedules the existing rebuild as correctness work through the claim. A verdict
counts only for the store lifetime it read, so a verdict over a store that was replaced
during the check schedules nothing. The host's verification slot is bound to the store
lifetime an attach or a rebuild opened, and a rebuild's discard unbinds it before the store
goes, so the judgment asks that binding which lifetime is current: the entry's own store
reading is recorded only where an engine is composed. A verdict of damage over the lifetime
still bound leaves the check owed: it comes due again after the lane's retry backoff, not
one interval later. A verdict the judgment takes schedules the rebuild, whose discard
unbinds the lifetime and its clock with it. A verdict the judgment sets aside, because the
entry's stance refused while the check ran, is therefore met again once the stance feeds.
An overflow's reconcile is such a stance, and it keeps the store, so the damage would
otherwise stand for an interval. A refused operation, such as an open
failure, a busy database or descriptor exhaustion, is not a verdict. It schedules no
rebuild, and the check next comes due at its normal interval.

The closing check of a from-zero build and of a rebuild is not a background unit. A build
verifies the store it built, on the writer it holds, before it clears the
under-construction mark, so that check is readiness work, and it stays in the build's leg
on the worker pool.

The verification may hold a lane thread for its whole duration, once an hour per
attachment. While it reads, the WAL cannot checkpoint past its snapshot. Its copy takes
about as much disk as the store from its write to the end of the copy's check. These costs are
accepted. Measured in a release build at 10,000 documents of 2 KiB, the unit takes about
0.6 s, of which the copy takes about 0.12 s and the copy's check about 0.13 s; the copy is
about 54 MB; and a second writer waits at most about 1 ms for a write while the unit runs.
At 2,000 documents the unit takes about 0.13 s and the copy is about 11 MB.

### Claim-bound slices stay maintenance legs on the worker pool

This section is amended. The death prune's units join the sweep and the rolling-heal slice,
and the leg's order and its longest unit are restated. Its other rules stand as ADR 0051
wrote them.

The rolling heal, the shadow sweep and the death prune need the vault's claim and its
attachment. Each runs as a **claim-bound slice** in a maintenance leg on the shared worker
pool. A slice of the shadow sweep is one sweep of the shadow home, which reads one
directory. A slice of the rolling heal keeps its existing work bounds: 64 enumeration acts,
64 checked entries and 1 MiB of hashed content. A slice of the death prune is one
death-prune unit, bounded by the logical rows it reads and the deaths it deletes
([When a death prune runs](#when-a-death-prune-runs)). A leg runs them in this order: the
sweep, then one rolling-heal slice, then death-prune units. The rolling-heal slice comes
before the death prune, so a leg whose budget the death prune would fill still runs the
slice. No maintenance leg runs any part of the store's verification.

Their waits stay as they are. A queued apply takes the claim before a maintenance leg
begins. The rolling-heal slice runs only over a quiet entry: one that publishes `Ready`,
owes no heal rung, and has no queued apply, no pending watcher facts and no teardown due.
The shadow sweep does not wait for a quiet entry, because a vault under continuous edits is
never quiet and the sweep would never run. A maintenance leg runs it whenever its clock is
due, quiet or not. The death prune does not wait for a quiet entry either, for the same
reason, and runs only where the store's link health is admitted. The architecture promises
that shadow cleanup and store verification keep their fairness under sustained watcher
traffic. The reconcile leg's hand-on to due maintenance, ahead of another reconcile turn,
keeps that promise for shadow cleanup, as it does today. The verification keeps it by
running on the lane, which reconciles do not hold back.

**A maintenance leg takes the background turn budget.** The leg checks the budget between
units and always runs at least one. A unit of a maintenance leg is one sweep, one
rolling-heal slice or one death-prune unit, each of which runs uninterrupted to its own
bounds; a leg takes at most one rolling-heal slice. A maintenance leg's units therefore
hold the claim and one worker slot for at most its budget plus one unit; the leg holds
them beyond that only to drain the watcher batches observed meanwhile, as every leg does
before it releases the claim. The longest unit is a rolling-heal slice, about 13 ms at
10,000 documents, and a death-prune unit's row and removal bounds are sized by measurement
so that its own work costs no more; a write-ahead-log checkpoint can land in any writer's
commit, a unit's included, and is not counted in a unit's cost. The leg counts its units beside its clock, as a background turn
does. A leg that ends with maintenance still due schedules another, as [Work starts at a
wake or a deadline](#work-starts-at-a-wake-or-a-deadline-with-no-poll-backstop) rules.

The shared worker pool now runs correctness work, readiness work and these maintenance
legs. A maintenance leg can therefore take a slot that an attach or a reconcile of another
vault needs, for at most its budget plus one unit. Long semantic work and the
verification never take a slot.

### Status reports embedding coverage

This section is amended. A census below the death floor keeps its last whole counts while it
counts again, and never starts that count over because of the floor, as a reconcile never
does. A count that reaches the end of both feeds records the write generation it read before
that final page. Its other rules stand as ADR 0051 wrote them.

`EngineStatus::On` gains `coverage: { embedded, total }` beside `freshness`. Both counts
are taken at one store state, the one the unit's page was read from or, while a count is
catching up, the one it last counted whole. They are written into the slot's report as each
unit ends, so a status never waits behind a background turn. `coverage` is `null` until a
unit has taken both counts whole; `freshness` is `null` until a background turn has
reported.

- `total` counts the documents the engine would embed in that store state. The names-only
  rows of unreadable documents are not among them.
- `embedded` counts those documents whose content in that store state has embeddings in
  the sidecar.

`freshness` stays the lag signal. The two answer different questions. A vault whose every
document is embedded, with one document edited 100 times since, reads as full coverage and
100 generations trailing. Search answers keep carrying `freshness` alone.

The counts follow the performance posture. A background turn's cost follows the rows it
moved, so a vault-wide comparison on every background turn is a defect. The engine keeps
the counts in its sidecar through the **census**, an embedding-free reader of the document
feed and the tombstone feed with positions of its own. The census keeps one row per
readable document with its body hash, and every row it moves and every embedding the drain
moves adjusts the two counts in the same transaction. Each unit reads at most one census
page of each feed, beside its drain page. A count that has more rows to read than that,
such as the first count of a vault or the count after a store rebuild, takes several units.
Until it is whole the status reports the counts the census last took whole, and the sidecar
records those last whole counts, so an engine opened again reports them too. A first
count has no whole count to report, and neither does a count over a new store epoch: the
census empties when the store epoch changes, so no count is carried from one database to
another. A census that has not read through the death floor empties and counts again too,
but over the same database, so it keeps its last whole counts, and the status reports them
until the new count is whole. It records the write generation its first page reads and, on
completing, that generation itself as read through, never starts the count over because of
the floor, and counts again where it completes with the floor above that generation,
keeping its last whole counts across the second count. A count that reaches the end of both
feeds, where that final page reads a floor the census has read through, records the write
generation it read before that page, as a completed drain records its watermark, and the
census's read-through generation counts that record
([The death floor](#the-death-floor)). `EngineStatus::On` is
`#[non_exhaustive]`, so the new field is an additive `norn-wire` change.

### Non-goals

- No real-model embedder. The stub stays the only one.
- No `norn-config` home for `background_threads` or `background_turn_budget`. Layer 6
  serving gives them one, as it does for the read budget.
- No change to how the per-entry claim orders lane-1 work.
- Reads stay unpreempted.

### Considered options

- **Keep semantic work on the claim.** A real-model page blocks that vault's writes for
  its whole duration. It also widens the window in which a `Ready` vault does not see a
  recent edit to a page of embeddings. Rejected.
- **Keep semantic work and the verification on the shared worker pool under a
  cap.** A cap still lets a job that can last seconds occupy a slot that an attach or a
  reconcile of another vault needs. Separate threads isolate the two by construction.
  Rejected. Claim-bound slices stay on the pool because they need the claim, and the time
  budget bounds the slot they hold.
- **Run every background unit on the lane, the claim-bound slices included, with a
  reserved-thread hand-on.** A leg would hand the claim to a slice only where it could
  reserve an idle lane thread, so a held claim never waited for one. Without the
  reservation, a held claim could wait for a lane thread, which can be inside another
  store's whole verification. The reservation needed its own wake protocol, between leg
  ends, claim releases and idle threads, and that protocol produced a lost wakeup.
  Rejected.
- **Let a background turn run only while its vault is quiet.** This is today's rule for
  semantic slices. Under continuous edits the vault is never quiet, so the engine never
  catches up, and nothing guarantees it ever will. Rejected.
- **Keep a poll tick as a backstop beside the wakes.** This includes the watcher poll
  that schedules due maintenance today. A backstop is a second path that hides a missing
  wake site. Tests keep passing while a site is missing, and the only symptom is latency
  up to the tick interval. Without it, a missing wake site shows up as owed work that does
  not start, which a test can see. Rejected.
- **Bound a background turn by work alone.** The cost of a unit depends on the embedder,
  so a work bound is wrong for one embedder or the other. Rejected.
- **Let the maintainer lock's release wait for the unit in flight.** This is sound, but a
  detach would then wait for a whole page of embeddings, estimated at seconds under a real
  model.
  Taking the slot lock bounds the wait by one sidecar step instead. Rejected.
- **Let the shadow sweep wait for a quiet entry.** Under continuous edits the sweep would
  never run, which breaks the fairness the architecture promises shadow cleanup. Rejected.
- **Run the whole verification off the claim.** Chosen. The full-text check is a write,
  so it needs the write lock of whichever database it runs on; a copy of the store, written
  by `VACUUM INTO` through a read-only connection, gives it a database of its own, so no
  part of the verification needs the store's writer.
- **Keep the full-text check on the claim, as a unit of the maintenance leg.** Measured in a
  release build at 10,000 documents of 2 KiB, the check took 92 to 140 ms, and a second
  writer waited 104 ms for a write while it ran: a hold at human scale. Rejected.
- **Check the index read-only, against a scratch index built from the bodies.** It needs
  no copy, but comparing the two indexes term by term took 3.8 s at 10,000 documents.
  Rejected.
- **Replace `freshness` with `coverage`.** Coverage does not measure lag, and search
  answers and the unfiltered vector rung's margin read lag. Rejected.

### Consequences

This section is amended. Its accepted costs name the death-prune unit beside the
rolling-heal slice, and the census's whole reads include its recount below the death floor.
Its other consequences stand as ADR 0051 wrote them.

The implementation lands in four sequential pull requests:

1. the lane, the background scheduler and their triggers, with maintenance legs started by
   deadline and the budget on maintenance legs;
2. semantic work off the claim;
3. the whole verification off the claim, its full-text check on a copy;
4. embedding coverage in status.

This decision merges as `Proposed` with the first and becomes `Accepted` with the last. On
acceptance, ADR 0048's status becomes superseded and links here. Its content does not
change.

**The architecture's contract follows.** Each pull request updates the passages of
`docs/architecture.md` that describe the behavior it changes:

- Part I, *The heal ladder*: the paragraph on silent damage, which runs the verification as
  lifecycle maintenance beside the shadow sweep. The whole verification becomes a
  background unit on the lane, with its full-text check on a copy of the store.
- Part I, *The trust model*: the rolling-heal bullet. Its maintenance slices stay
  maintenance legs, bounded also by the background turn budget and started by a deadline
  or a leg end rather than a poll. Its promise that shadow cleanup and store verification
  keep their fairness under sustained watcher traffic names the reconcile leg's hand-on
  that keeps it.
- Part II, the crate map's `norn-semantic` row: the engine opens in a background unit, not
  a lifecycle slice, and its drain runs on the background lane through the delivery's feed
  connection, not in quiet maintenance.
- Part II, the crate map's `norn-host` row: the background lane beside the worker pool,
  the background scheduler with its wakes and deadlines, owed semantic work and
  verification as background units rather than one quiet-maintenance unit per leg,
  maintenance legs started by a deadline or a leg end and never by a poll, the install
  rule's test of delivery and store lifetime rather than of the claim, and the coverage
  field in engine status.
- Part III, *Request flows*: the settling read's window under a maintenance scan becomes
  the window under one maintenance leg bounded by the background turn budget. The apply
  bullets that name a maintenance scan name that bounded leg.

These consequences are testable:

- No semantic background turn feeds while its entry's stance is refuse.
- Owed lane work starts without a poll tick.
- A due claim-bound slice starts without a poll tick: at once over a free claim, and at
  the claim's release where the claim was held when the slice came due, including after an
  apply that hands on no reconcile.
- On a vault no leg touches, the second hourly deadline after attach starts a maintenance
  leg, as the first did.
- The scheduler runs its ask at most once per wake or deadline it receives.
- A detached entry, or one whose release is in flight, contributes no deadline and gets no
  maintenance leg.
- A watcher poll never schedules a maintenance leg.
- A background turn ends within its budget plus one unit, and a budget too small for any
  unit runs exactly one unit per background turn.
- A maintenance leg ends within its budget plus one unit, and a budget too small for any
  unit runs exactly one unit per leg.
- One vault with a large backlog cannot starve another vault's background turns.
- No worker-pool job runs a background unit.
- An apply completes while its vault's semantic background turn is in flight.
- A write completes while its vault's verification runs, the copy and the copy's
  full-text check included.
- No maintenance leg runs any part of the verification.
- The verification's copy is removed on every way out of it, and a copy a crash left is
  removed at the next attach or before the next copy.
- Under continuous edits, the shadow sweep runs within one reconcile turn of coming due.
- Each refusal of the install rule holds: a disable, a reload that replaces the section, a
  detach followed by a re-attach, and a store replacement each refuse a late
  initialization.
- A background turn stops at the first unit boundary after its delivery or store lifetime
  retires, and a unit that finds it retired commits nothing.
- A verification verdict over a replaced store schedules nothing.
- A detach that meets a semantic unit in its embedding phase returns without waiting for
  the embedding, and that unit commits nothing.
- A search waits behind at most one unit, and under continuous search the lane still runs
  its units.
- Host shutdown waits for at most one unit for each lane thread, and interrupts a
  verification in flight.
- `coverage` equals the true counts at the snapshot of the last whole count.
- A settled drain equals a from-zero recompute.

These costs are accepted:

- Background work and correctness work compete for CPU. The lane's thread count bounds
  that competition.
- With the production value of one thread, a verification delays every other derived
  store's background turns for the length of one verification.
- Each verification writes a copy of the store into its derived directory, about as large
  as the store, which stands from its write to the end of the copy's full-text check.
- Each enabled engine holds one feed connection outside the request read budget.
- A maintenance leg holds the claim and one worker slot for at most the background turn
  budget plus one unit. By its own work a rolling-heal slice is the longest such unit, and a
  death-prune unit is sized so that its own work costs no more; a write-ahead-log checkpoint that lands in a
  unit's commit is not counted, as it is not for any other writer.
- A semantic background turn's overrun is one page and the census pages read beside it.
  The real-model runtime will have to size the page so that one page stays within human
  scale.
- The census keeps one sidecar row for each readable document.
- The census reads both feeds whole, without embedding, at a vault's first drain, after
  every new store epoch, such as a store rebuild, and after it falls below the death floor.
  Measured on the first full drain, that pass adds about 20 ms at 2,000 documents and about
  200 ms at 10,000.
- The census's work for one settled edit is one row. Measured, the unit that drains one
  settled edit grows from about 0.27 ms to about 0.55 ms.

## From-zero build prices

Measured on a lab prototype in release builds, as medians of a serial matrix run in
Williams order, on a vault's first build over a 10,014-document fixture unless stated,
against a build serving early reads from ADR 0043's disposable sources. Each figure is end
to end, early reads served; ADR 0047's link-health figures come from a phase profile of
construction alone and are not comparable with these.

| Measure | Disposable sources | Built in steps |
|---|---|---|
| Ready | 30.6 s | 9.0 s |
| CPU time | 27.9 s | 10.0 s |
| First Get by name | 0.68 s | 0.42 s |
| First Get of a document with links | 0.74 s | 0.43 s |
| Delivery of a request arriving mid-build | 69 ms | 46 ms |
| First field Find | 4.2 s | 3.6 s |
| Full-text search available | after Ready | 9.0 s |
| Files opened | 20,055 | 10,015 |
| Peak resident memory | 23.6 MiB | 27.8 MiB |
| Ready at 2,014 documents | 5.15 s | 2.05 s |

Warm reads are equal. The finished stores are equal under the store's semantic audit,
in schema text and in full-text integrity, and — since no fact arrived during the measured
builds — in document ids.

### Known limit

`CREATE INDEX` packs index pages about 99% full, where row-by-row maintenance leaves
them about 88% full, so the first writes after a from-zero build split pages. Each costs
about 1–2 ms more, and about 4 ms for a rename of a hub, for roughly the first 200
writes scattered over the vault or about 5 writes to one document: about 0.3 s in total
at 10,014 documents. The packing survives restart. Per-write cost then matches or beats
a store whose indexes were maintained row by row. SQLite has no fill factor, and VACUUM packs pages full;
maintaining indexes during construction removes the cost at 0.7 to 2.2 s more before
Ready. The cost is judged imperceptible and accepted.

### From-zero build alternatives

- **Keeping disposable sources.** Every document an early source serves is derived
  twice, and Ready moves later.
- **Building in steps for a vault's first build only.** A rebuild of an existing store
  would keep a second construction path from empty, refuse reads until `Ready`, and pay
  per-changeset link-health re-decision across every chunk.
- **Withdrawing every early capability on any accepted fact.** One edit anywhere in the
  vault would hold every early read until `Ready`, though it touches one document.
- **A document-by-document build followed by one whole-vault link-health pass,
  without writing names first.** Early reads would still need rows written twice, and it does not let
  a requested document be fetched and parsed on its own.
- **A separate build-time name table standing in for documents during resolution.**
  It writes every name twice.
- **Reusing the vault-file table for pending documents.** Its key and eligibility rules
  differ from a document's.
- **Waiting for vault-wide content before any Get.** The first Get took 9.6 s at 10,014
  documents in the prototype.
- **Writing provisional link targets early.** An early answer is exact or not given.
- **Keeping full-text triggers during construction.** They cost about 2 s at 10,014
  documents inside the content step and delay Find.
- **Maintaining indexes during construction.** It costs 0.7 to 2.2 s more before Ready
  to avoid the page-split cost above.
- **A larger build cache.** It held 76 MiB resident to save 0.08 s.
- **`synchronous=OFF` during construction.** A power loss can corrupt the file; raising
  the autocheckpoint keeps durability for most of the gain.
- **VACUUM after construction.** It packs pages full, which is the cost it would be
  meant to remove.
- **Making the whole partial store publicly readable.** Completeness would depend on
  which rows happened to arrive.
- **Serving directly from files.** It would duplicate query, resolution and lifetime
  rules.
- **Calling a partial store `Ready`.** It would hide unfinished canonical work.

## Alternatives

The alternative was the pin as a teardown veto — an entry that cannot be torn down while
any read is in flight, which is what the first of these records asserted. It loses on what
a read holds: nothing. What defers the give-back at a refusal or a destruction is coverage
custody, not the pin — the leg is out with the entry's coverage and the give-back is the
leg's own end; a read holds no coverage, so at those sites the entry is still holding its
own and the refusal or the destruction is itself what reaches detach, under the live hold
rather than at a leg's end. Granting a read that deferral would put reader count in front
of exactly the moves that must make progress — a conflict being refused, a host coming
down, a maintainership already lost — while buying nothing the shared handle does not
already carry. The alternative to the lease was to refuse a read while a detach stands
scheduled. It answers the same safety question and loses twice: it makes a vault unreadable
for the length of a teardown the caller's own demand would have cancelled, and it would be
a second rule about a demanded entry in a lifecycle that already has one.

Four alternatives to the wait were rejected:

- **Keep refusing.** It pushes the retry onto every client, for every edit.
- **Answer only from `Ready`.** Under steady edits no `Ready` is published, so every read
  that meets the vault starves until its bound and refuses.
- **Answer from the last `Ready` snapshot.** That answer is stale, and a read that answers
  stale breaks the trust the trust state exists to carry.
- **Check the watcher on every read.** The substrate maintains trust by signal and by
  schedule, never per request.

Three alternatives to holding back a recovery only a change can answer were rejected:

- **Keep restarting it, and let the warming answer carry the prior cause.** The host still
  runs a full recovery per read that cannot succeed, and the read is still refused as not
  ready rather than with the cause the client has to act on.
- **Fingerprint the control files on every read that meets such an entry.** It is a
  per-request check where the substrate maintains trust by signal, and the watcher already
  carries the signal.
- **Hold back every recovery a read meets.** An environmental refusal or a lost watcher can
  heal outside the vault's bytes, where no fact the entry takes in says so, and a read
  workload would then never heal it.

## Elastic read capacity

`norn-store` owns the shared read budget and its cache metadata. Production host
composition supplies its validated `ProductionPolicy.read_budget`. There is no
serving configuration parser yet. The Layer 6 serving composition reads any persisted
operator override through `norn-config` as machine-local state, never vault config.
The host configures one shared read budget. Pools grow on demand within that budget,
retain a small warm working set across a burst, and shrink to zero reader connections
after an idle grace period. Idle capacity can be reclaimed immediately when another vault
needs it. Trimming also progresses without incoming requests, on the existing dispatcher
tick. Writer and watcher lifetimes keep their existing rules.

Capacity counts actual host-request connections. A plain read reserves one connection;
a multi-key continuation reserves its source and scratch connections atomically
before taking its source snapshot. A fresh multi-key first page prefers that pair.
At its turn, it first reclaims idle capacity. If only one connection can be granted
because the rest are active, it takes a complete source-only grant and follows the
existing tuple-only first-page fallback. It never waits while holding a partial pair.
Capacity is at least two. The initial production policy admits four
connections and retains idle connections for five seconds; both values are configurable.
Warm source and result connections participate in the grant, so reserving scratch capacity
does not first discard the same warm result reader. Source writers, worker-bounded private
apply readers, and semantic sidecars retain their separate resource bounds.

When capacity is exhausted, waiting derived stores take turns; requests within a
store keep arrival order. The fairness key is the derived database path, not the
vault root: two registrations over one root have independent turns under ADR 0016. A vault may borrow spare capacity while no other vault waits. An idle
connection fast path cannot bypass an owed turn. Running reads are never preempted, and
all acquisition waits share the original deadline. Fairness promises a turn when capacity
returns, not completion while other reads hold it indefinitely. A mandatory two-slot
request whose turn arrives with only one slot available waits for the second without
holding the first. Later requests cannot bypass that turn, so the free slot may sit
unused. A fresh first page can instead take its source-only fallback at that turn.

Admission covers the complete bounded working set a request may need. A request never
holds one allocation while waiting for another allocation from the same exhausted budget.
Opening connections and connections still held from retired pools remain charged. Capacity
is released only after the corresponding resource has closed. Registry and budget locks
cover bookkeeping only; they span no SQL, database open, or file close. The entry gate
may take the budget metadata lock for a nonblocking attempt. Budget and result-registry
code never takes an entry gate, including while granting or waking a waiter.

### Capacity alternatives

A single reader keeps resource use small but serializes unrelated queries and pages.
A fixed pool per vault removes some contention while retaining capacity in proportion
to registrations, including idle vaults. Both fail the desired elastic resource rule.
An unbounded pool removes capacity waits by removing the resource bound. The shared
budget instead bounds actual connections while letting one busy store borrow capacity.
The former measured-contention trigger for adding readers is replaced by this bounded
concurrency contract. Performance qualification measures its costs, not whether to
permit independent reads at all.

## Prices

Each open reader carries a SQLite page cache, a prepared-statement cache and descriptors.
Each active source snapshot pins the write-ahead log against checkpointing; checkpointing
stays passive, preserving the reader's never-blocks-the-writer guarantee. The bounded read
shapes usually keep those pins short. Overlapping snapshots under sustained edits can
prevent the log from resetting even when each read is bounded. WAL bytes can therefore
grow until a gap permits checkpoint progress. No reader is preempted to force that gap.
Population time is not bounded by the acquisition deadline. At capacity two, one
population can use the whole budget; at the default four, two can do so. Other reads
may then reach their deadline and refuse. There is no reserved service class for light
queries. Increasing the configured cap trades memory and descriptors for concurrency.
Rotating across more active stores than the cap forces cold opens despite warm reuse
within a burst. The native memory and descriptor bars price the configured working
set, including result readers and work being opened or retired.

On SQLite's Unix backend, closing a reader can leave its main-file descriptor retained
while another connection keeps that file locked. Reopening can reuse that descriptor.
Idle shrinking therefore releases connection memory and WAL descriptors without promising
that every main-file descriptor closes before detach. Resource accounting and qualification
include these retained descriptors across attached vaults. The lifetime rule prices one more: a read that a throwaway
teardown runs under finishes against files the teardown has unlinked, and its descriptors
ride until the hold drops. The lease prices another: a vault under continuous read
traffic is a vault the idle budget stops bounding, which is what the idle interval is for, and the
lease's restart on drop is what keeps that honest. The wait prices another: a read that
meets a change holds its caller for up to the bound, and its demand holds the entry's idle
interval open for as long as it waits. Answering before `Ready` prices another: an
answer can be taken while later facts are still being derived; it says so in the trust it
carries, and a client that needs those later facts must read that trust rather than assume
`Ready`. Holding back a recovery only a change can answer prices the last one: a declaration
corrected by a change the watcher never reports is answered to reads by its old cause until
another change arrives or a client demands the vault, and the watcher's contract to
over-report freely and under-report never is what keeps that window closed.

## Retained count results

### Context

A count grouped by several keys orders arbitrary tuples. The store's per-key
indexes cannot hand those tuples back in that order. Repeating the aggregation
on each page makes a full drain quadratic when the groups grow with the input.
Walking one member within a fixed prefix improves some distributions, but does
not establish bounded drain work across arbitrary distributions. Maintaining
every possible grouping would move combinatorial storage and maintenance onto
document writes.

### Decision

A multi-key count aggregates complete tallies once and streams them into a
disk-backed temporary result. Further pages seek that result in tuple order.
The result belongs to the store that admitted it and is shared by that store's
readers. It retains neither a source snapshot nor a read demand lease between
requests. Source readers keep their existing read-only protection.

Independent requests can page the same completed result concurrently. Result paging
holds no application lock across SQL. Result readers participate in bounded resource
admission; no result wait can deadlock a request that already holds source capacity.
Requests for the same selection and source view share an in-progress population rather
than repeating aggregation. Source view means store epoch, committed write generation
and schema pin. A follower keeps its own complete grant and source snapshot while it
waits, under its original acquisition deadline. The leader already owns its complete
grant and never waits for budget capacity, so followers cannot prevent its progress.
The follower's occupied slots and WAL pin are an accepted cost of sharing. A fresh
source-only fallback does not join that population. A failed or cancelled population releases its reservation
and wakes waiters; no incomplete relation is published.

Publication follows successful construction. Read access after publication is read-only.
Expiry removes the result from admission without invalidating a reader already using it;
the existing expiry check before an answer still applies. Storage and quota accounting
outlive every reader that still holds that storage. Disposable storage cleanup covers
failed builds, normal release and process interruption. Sweeps do not delete files
under another active maintainership. Result readers never busy-wait: an immutable result has no legitimate writer, so
external lock contention refuses transiently. A transient connection or resource failure does not condemn a healthy
result; actual result damage retires that result and never the source database.

Each result has a private named file beneath its store's derived data directory, on the
norn data filesystem. Exclusive creation, a private directory and restrictive file modes
keep independent builds isolated. Construction uses its own writer and completes before
read-only readers can use the file. Reopening checks file identity; a missing or replaced
file expires its result. Result readers keep the storage owner alive until they close.

Host initialization completes its sweep before the same host can attach any store.
It sweeps recognized names only after acquiring the corresponding store's maintainer
lock, skipping stores another maintainer holds. An old store instance whose
maintainership ended is retired even if a read still owns its result file. A new
maintainer may unlink that file. Already-open readers finish on their handles;
an attempt to reopen the old name expires. No process-wide live-owner registry is
needed, and teardown never waits for those reads. Attach and rebuild also
sweep under maintainership. Cleanup failure is recorded and does not withdraw source trust;
ordinary retention failure and first-page fallback rules still apply. Normal expiry and
last-reader cleanup do not wait for another startup. A crash may leave files until the
next successful sweep; sweeping reads no vault document and no source database.

A result has an absolute deadline and explicit memory, descriptor and admission bounds. An admitted
result is retained within those bounds; unsuccessful retention admission follows the
first-page and continuation rules below. An admitted result is
not evicted to admit another request before its deadline. An expired or lost
retained continuation refuses and requires restarting the count; it never silently
repeats aggregation. Restart, detach and
rebuild may end retained results. Replaying and branching an unexpired cursor envelope
remain valid, including after another branch reaches the final page.

A retained continuation reads the original result, even after source documents
or the schema change. A fresh count reads current data. Every count report
names its `counted_at` source snapshot; the outer answer reading still describes
the current request's trust and acquisition. This is an explicit exception to
the data-freshness rule restated above for retained multi-key count continuations only.
All the acquisition, trust, lease and teardown rules remain in force, as do its
freshness rules for every source query. The reader, lease, stance, bound and prices are restated above; this section preserves
the retained-result exception.

The retained cursor envelope names the result beside its source snapshot; its
key remains the tuple labels. Its page size may change. Original typed orders,
reports and advisories stay with the retained result rather than being judged
under a later schema. A lost result expires, including after store replacement,
regardless of source movement. Complete-tally labels, typed equality, null
ordering, list cross products and distinct-document counts keep their meaning.

A tuple position reused under different grouping or predicates starts a count
under that request, with the existing order and width checks. It cannot read
the previous request's retained rows. A legacy tuple continuation may admit a
retained result once. When retention cannot admit a fresh first page, that
page keeps its existing source-query behavior and returns a tuple-only
continuation. Any scratch failure on a fresh first page follows this fallback,
including inability to create the result file. A continuation reports the failure
with guidance to restore temporary storage or process resources and retry.
Retention capacity is never reported as an empty or partial
successful count. A continuation unable to establish retained state refuses
explicitly. Every admission refusal is distinct from expiry and says whether retrying
the same continuation can succeed. Only expiry calls for a restart. The improved drain guarantee applies to admitted retained results.

The source aggregation uses the existing count composer and filter fragments.
Scratch failures are result failures and never evidence that lane-1 is damaged.
Cost accounting includes source aggregation, result population, and result
paging. Acceptance includes bounded memory for SQLite's sorter as well as its
table cache. Source contributions and key bytes govern population cost; the
number of distinct output groups alone does not describe it. Optional retained-table disk quotas do not bound source aggregation spill
files, which remain ordinary SQLite query working storage. There is no
additional result-size cutoff by default: disk usage follows the result bytes,
subject to the filesystem and SQLite limits. An operator may configure per-result
and total disk quotas; those can refuse admission and identify whether to
retry after capacity becomes available or increase the configured quota where the
cause is established. SQLite cannot distinguish an exhausted page cap from an
exhausted filesystem; an ambiguous storage-full failure names both remedies and
allows retrying the continuation. File descriptors are priced per active result
alongside memory and disk. Inspection through `count_plans` does not retain a
new result whose report is discarded.

### Consequences

The first multi-key page may do more work before it can answer. Subsequent
pages avoid re-aggregating the unreturned suffix. Zero-key and single-key
counts keep their existing execution path. No permanent grouping projection
is maintained on document writes.

An evictable cache with recomputation would preserve indefinitely reusable
tuple-only cursor envelopes, but would guarantee the improved cost only while its entries
survive. Explicit expiry makes the retained lifetime an enforceable contract.

The retention deadline is an upper bound. Idle detach, restart and source-store
replacement may end a result earlier because retention holds no demand lease.
The next multi-key count prunes expired results before retention admission and retires
their idle readers immediately. Active readers keep storage and its quota charge until
they close. Last-reader cleanup also follows normal release or store teardown. The
dispatcher tick trims idle connections but introduces no background result-expiry timer.
A fresh request reuses a result only when at least half its configured lifetime
remains; reuse never renews the deadline. Result coordination never waits under
the entry gate or through teardown. Teardown drops ownership without waiting
for a population or result reader; an already-running read finishes on its
own handles.

A continuation whose envelope names the same retained result reports no movement.
A continuation that admits a result reports movement from its envelope's snapshot
to `counted_at`, by the ordinary rules.
Source movement is visible by comparing `counted_at` with the outer answer
reading. Changing selection is detected from its canonical digest in the cursor
envelope even when the old result is gone; it follows ordinary tuple-position
judgment under the new request rather than reading the old relation.

The prices added to the reader contract include result bytes on disk, bounded caches
and descriptors for concurrent result access, and source sorter working storage during
population. Result retention and reader connection admission have explicit bounds.
Source snapshots and WAL pins end with each request. Native process memory and
descriptor pairs cover the fixed working set across vault sizes; Rust allocation
counts alone do not see SQLite. `count_plans` records real execution but does not
leave a new retained result behind when its report is discarded.
