---
status: superseded
superseded-by: 0051-a-schema-reload-serves-reads-again-when-its-activation-ends.md
date: 2026-11-07
---

# 0050 — Background work that needs no claim runs on its own lane, claim-bound maintenance runs in legs bounded by time, both start only when owed, and a read can miss a moment-old edit for one maintenance leg or one reload leg

Supersedes [ADR 0049](0049-background-work-runs-on-its-own-lane.md) and restates it whole.
ADR 0049 restated ADR 0048 whole and amended two of its sections: whole-vault and semantic
readiness, whose scheduling of semantic work the section
[Background work runs on its own lane](#background-work-runs-on-its-own-lane) replaced, and
the known limit of *A read waits for the facts it met*, which it confined to one
maintenance leg bounded by the background turn budget. ADR 0048 restated ADR 0045 whole
and changed one rule, that each requeue takes once, which the section
[A step ends at its closing take](#a-step-ends-at-its-closing-take) records. This decision
amends one of ADR 0049's sections and copies every other section as ADR 0049 wrote it:

- **The known limit of *A read waits for the facts it met*.** A reload leg opens the
  observation gap a maintenance leg opens, so the gap is limited to one maintenance leg or
  one reload leg. The reload leg's window lasts from its start to its end drain, and a
  schema-changing activation closes it when it publishes warming.

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
construction [ADR 0047](0047-a-write-re-decides-only-the-links-whose-inputs-it-moved.md)
judges link health once over, under the same term. Whole-vault readiness retains its
existing meaning.

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
changes or when the entry has derived further through the facts it has taken in, takes the
gate again, and reads the published demand afresh.

**A settling read waits for the facts it met, never for the ones that arrive after it.**
Under the first hold that finds the entry settling, the read records how far the facts the
entry has taken in run at that instant, and it never moves that record. It establishes its
snapshot, in the same critical section that reads the published demand, at the first of two
things it observes: the entry `Ready`, or the entry having derived every fact up to that
record with a reader standing beside it. Either way the snapshot holds every fact the entry
knew of when the read met it, so **it is never older than the change the read arrived
during**. A vault schema reload waits for `Ready`, because it replaces the pool: a read it
settles answers from the pool installed once `Ready` returns. **A settling read that finds a
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
reaches first, and one sweep step. That wait is shorter than the
maintenance leg a read could wait behind under ADR 0048, which ran the whole verification
under the claim. The wait stays inside the read's bound.


## A from-zero build writes once, in steps

### Scope

A **from-zero build** constructs a canonical store from empty, and every one is built as
this section rules: a vault's first build, and every rebuild of an existing store from
zero — over a DDL fingerprint, derivation version or path order the store does not match,
over damage, or by any other rung that discards the database and derives the vault again.
It is the construction [ADR 0047](0047-a-write-re-decides-only-the-links-whose-inputs-it-moved.md) judges link
health once over, under the same term. There is one construction path from empty to
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
4. **Relations.** Link health is judged once, over every eligible link, as ADR 0047 rules.
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
take are then reconciled as ordinary changesets, under ADR 0047's per-changeset rule and the
restored autocheckpoint; and `Ready` is published.

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
  again before step 4 ends, as ADR 0047 rules. After step 4's closing take nothing is
  requeued: the fact waits for the reconcile that precedes `Ready`.
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
  the links beneath it are judged again, as ADR 0047 rules. After step 4's closing take the
  fact waits for the reconcile that precedes `Ready`, whose job-end prune ends the finding and re-decides those
  links.

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
([ADR 0047](0047-a-write-re-decides-only-the-links-whose-inputs-it-moved.md)), and a process killed after
that commit leaves a finished store the attach heal converges. The statement that the
full-text triggers are the only writer of the full-text index gains the construction
exception above. The permission-loss row changes as
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
- for a maintenance leg, a rolling heal due or a rolling heal pass still in progress, and a
  shadow sweep's age threshold reached.

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
correctness, as [ADR 0047](0047-a-write-re-decides-only-the-links-whose-inputs-it-moved.md)
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

A semantic background turn reads the change feed through its own read connection, never
through the attachment's `Store`. It embeds while holding no store and no lock, and it
commits to its own sidecar. It takes no claim and no attachment, so it runs alongside that
vault's reconciles and applies. They share only CPU, which the lane's thread count caps.
Embeddings therefore converge while edits continue.

Correctness rests on mechanisms that already exist. Cursors are consumer-owned feed
positions, each a `generation:path` pair, and valid within one store epoch. The sidecar
records the store epoch it observed beside its cursors (`OBSERVED_STORE_EPOCH`). A feed
whose epoch differs from that record makes the drain reconcile the sidecar against the
store before it reads the feed: the rescan ADR 0047 rules for an epoch change. The drain's
generation check already keeps a hash from being written beside values it does not
describe, even against a writer interleaving through another store handle. The feed-read
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

The rolling heal and the shadow sweep need the vault's claim and its attachment. Each runs
as a **claim-bound slice** in a maintenance leg on the shared worker pool, as today. A
slice of the shadow sweep is one sweep of the shadow home, which reads one directory. A
slice of the rolling heal keeps its existing work bounds: 64 enumeration acts, 64 checked
entries and 1 MiB of hashed content. A leg runs them in today's order: the sweep, then one
rolling-heal slice. No maintenance leg runs any part of the store's verification.

Their waits stay as they are. A queued apply takes the claim before a maintenance leg
begins. The rolling-heal slice runs only over a quiet entry: one that publishes `Ready`,
owes no heal rung, and has no queued apply, no pending watcher facts and no teardown due.
The shadow sweep does not wait for a quiet entry, because a vault under continuous edits is
never quiet and the sweep would never run. A maintenance leg runs it whenever its clock is
due, quiet or not. The architecture promises that shadow cleanup and store verification
keep their fairness under sustained watcher traffic. The reconcile leg's hand-on to due
maintenance, ahead of another reconcile turn, keeps that promise for shadow cleanup, as it
does today. The verification keeps it by running on the lane, which reconciles do not
hold back.

**A maintenance leg takes the background turn budget.** The leg checks the budget between
units and always runs at least one. A unit of a maintenance leg is one sweep or one
rolling-heal slice, which runs uninterrupted to its own work bounds; a leg takes at most
one rolling-heal slice. A maintenance leg's units therefore
hold the claim and one worker slot for at most its budget plus one unit; the leg holds
them beyond that only to drain the watcher batches observed meanwhile, as every leg does
before it releases the claim. The longest unit is a rolling-heal slice, about 13 ms at
10,000 documents. The leg counts its units beside its clock, as a background turn
does. A leg that ends with maintenance still due schedules another, as [Work starts at a
wake or a deadline](#work-starts-at-a-wake-or-a-deadline-with-no-poll-backstop) rules.

The shared worker pool now runs correctness work, readiness work and these maintenance
legs. A maintenance leg can therefore take a slot that an attach or a reconcile of another
vault needs, for at most its budget plus one unit. Long semantic work and the
verification never take a slot.

### Status reports embedding coverage

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
another. `EngineStatus::On` is `#[non_exhaustive]`, so the new field is an additive
`norn-wire` change.

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
  budget plus one unit. A rolling-heal slice is the longest such unit.
- A semantic background turn's overrun is one page and the census pages read beside it.
  The real-model runtime will have to size the page so that one page stays within human
  scale.
- The census keeps one sidecar row for each readable document.
- The census reads both feeds whole, without embedding, at a vault's first drain and after
  every new store epoch, such as a store rebuild. Measured on the first full drain, that pass
  adds about 20 ms at 2,000 documents and about 200 ms at 10,000.
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
