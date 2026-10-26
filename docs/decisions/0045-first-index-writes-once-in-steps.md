---
status: proposed
date: 2026-10-26
---

# 0045 — A from-zero build writes each document once into the canonical store, in steps, and serves each capability when its data is complete

Supersedes [ADR 0043](0043-first-index-complete-reads.md).
The elastic reader budget, canonical read acquisition, demand lease, recovery
restraint, settling bound, teardown, retained-count rules and whole-vault and semantic
readiness are restated below. The change replaces how a store built from empty serves
early requests, and which builds do. ADR 0043 derived request-specific disposable
sources beside the canonical store and answered from them, during a fresh store's first
attach only; this decision builds the canonical store itself in steps — on every
**from-zero build**, a vault's first build and every rebuild from zero alike — writing
each document's row once and filling it progressively, and switches each capability on
when the data it reads is complete. A from-zero build is the construction
[ADR 0044](0044-a-first-build-judges-link-health-once.md) judges link health once over,
under the same term. Whole-vault readiness retains its existing meaning.

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

While a background maintenance slice holds the entry, the entry stays `Ready` and does not
yet see an edit made moments before, so a read in that window can miss that edit. This
accepted observation gap is limited to one bounded slice, including a semantic slice;
watcher batches drain before another slice starts. An entire semantic initialization
backlog must not become one such interval. The bound is work-based, not a promise of
wall-clock duration for an individual filesystem or model operation.


## A from-zero build writes once, in steps

### Scope

A **from-zero build** constructs a canonical store from empty, and every one is built as
this section rules: a vault's first build, and every rebuild of an existing store from
zero — over a DDL fingerprint, derivation version or path order the store does not match,
over damage, or by any other rung that discards the database and derives the vault again.
It is the construction [ADR 0044](0044-a-first-build-judges-link-health-once.md) judges link
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
A crash during construction therefore restarts construction from zero and never resumes
over what the crash left. A store whose recorded inputs match and which carries no mark is a
finished store, and its open is the ordinary one.

### The canonical store is written once, in five steps

A from-zero build writes each document's row once and fills it progressively. Nothing is
written to a store it does not stay in. The steps run in order over the vault, each in path
order:

1. **Names.** Every document row is written with its names only, in path order, so
   document ids equal those of a store built document by document; every vault-file row is
   written final. A `.md` path whose bytes prove not to be a document is removed later
   through the ordinary removal path and leaves a gap in the ids, which is accepted. The
   name-resolution indexes are created at the end of this step.
2. **Content.** Each file is read once: its content hash, frontmatter projection, fields,
   and frontmatter link and tag facts are written, and its body is stored but not full-text
   indexed. The field indexes are created at the end of this step.
3. **Structure.** Each stored body is parsed. No file is read again: every fact comes from
   the bytes step 2 hashed. Headings, blocks, body links and inline tags are written. The
   indexes their readers seek are created at the end of this step.
4. **Relations.** Link health is judged once, over every eligible link, as ADR 0044 rules.
5. **Full-text.** The full-text index is built from the stored bodies in bounded batches.
   The transaction that ends the step installs the full-text triggers and clears the
   under-construction mark.

**Construction ends at step 5, and `Ready` follows in one order.** Step 5 installs the
triggers; the facts accepted since step 5 began are then reconciled as ordinary
changesets, under ADR 0044's per-changeset rule; a checkpoint runs; and `Ready` is
published.

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
  health, read as still loading until step 4 ends: resolution and health switch on
  together, and a missing-anchor verdict needs the headings and blocks of every document,
  which only the end of step 3 completes. Whether a path is a document is settled for
  every path when step 2 ends. A projection of the document that reads other documents —
  its backlinks among them — is admitted when step 3 ends, and until then reads as still
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

"Every document" counts the paths a filesystem fact has requeued: a vault-wide capability
is admitted only once each of them has run again through the step it needs.

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
phase and counters and the message "this vault is still indexing for the first time".
Resource and gate-acquisition failures keep their existing refusal rules. This adds a
price: an eligible caller can be held up to the ordinary bound, whereas a canonical request
that meets a from-zero build still refuses at once.

### A filesystem fact withdraws only what it touches

A from-zero build is covered by one watcher authority captured before enumeration. A
relevant filesystem fact the authority accepts while the build runs withdraws only what it
touches, until its path is derived again:

- **Its path is derived again.** During steps 1–4 the path is requeued at the front of the
  queue and runs again through the same step routines, up to the step the build has
  reached; a path requeued while step 4 runs has the links it reaches judged again before
  step 4 ends, as ADR 0044 rules. From step 5 on nothing is requeued: the fact waits for
  the reconcile that precedes `Ready`.
- **A Get of that document waits** until its path is derived again. Other Gets are
  unaffected.
- **Every vault-wide capability waits for it** — field Find, the step-3 shapes, link health
  and search — until every requeued path has run again, and from step 5 on until the
  reconcile.
- **A create, a delete or a rename changes name rows at once** during steps 1–4, through
  the ordinary add and remove path, and a Get whose name could resolve to an affected name
  checks its resolution again before it answers. From step 5 on, such a fact waits for the
  reconcile, and a Get whose name could resolve to an affected name waits with it.
- **Lost coverage withdraws every early capability.** An overflow, a lost root, a backend
  the watcher cannot cover, or any other loss of watcher fidelity withdraws every early
  capability, and neither draining a batch, finishing a heal window nor seeing a missing
  root return restores them. Construction continues, the store is reconciled with the
  vault before `Ready`, and requests meanwhile keep the ordinary loading behavior.

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
maintainer. No body changes while they are absent: a requeued path rewrites its body only
during steps 1–4, before step 5 has indexed any body, and every later body change — the
reconcile that precedes `Ready`, and the writes and repair admitted only at `Ready` — is a
changeset that runs after the triggers exist. A body changed after the index took it,
without the triggers, would leave the external-content index holding text the body no
longer holds and dropping text it never held. The finished store's schema equals an
ordinary store's.

### Construction keeps its cost following the work

- Each non-unique secondary index is created and committed at the end of the step after
  which its first reader is admitted, before that capability publishes, so an admitted
  statement's `EXPLAIN` gate sees the index; none is maintained row by row during
  construction. Unique indexes and primary keys stay.
- An admitted early statement never scans a vault-sized table.
- Host chunks stay bounded at 128 documents. A SQLite transaction spans up to 2,048
  documents, and commits at the next chunk boundary whenever a request is pending, so the
  request's snapshot sees its document.
- The WAL autocheckpoint is raised during construction, and the checkpoint before `Ready`
  runs in the order construction states. `synchronous` and the cache size keep their
  production values.

### Initializing a vault runs its first build in the foreground

Initializing a vault runs its from-zero build before it returns, so normal use has no cold
start; it returns once the vault is `Ready`. Early admission serves a vault used before its
initialization finishes. An environment error or an interruption that ends the build leaves
the vault registered: initialization reports the failure, and that indexing restarts from
zero on the vault's next use. A file the build cannot read is quarantined as one document,
under a finding naming it, and is not an initialization failure.

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
from-zero build now admits them. A crash during any from-zero build restarts it from zero,
where a rebuild of an existing store resumed by heal over the chunks it had committed.

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

## Whole-vault and semantic readiness

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
failure is published only for the still-current configuration identity, store lifetime
and lifecycle claim. Disable, reload, replacement and teardown retire obsolete pending
work; a late result cannot install an engine for an older declaration or retired store.
Its resources close outside the entry gate. No model or sidecar I/O runs under that gate.

Semantic initialization, store-epoch reconciliation and feed consumption run in bounded
lifecycle-owned slices, with watcher batches drained between slices. Vault-sized sidecar
reconciliation is not exempt from that work and memory bound. Semantic slices yield to
owed heal work, queued applies, pending watcher facts and due teardown, as other quiet
maintenance does. Outstanding semantic work does not defer idle detach; remaining work
can resume after a later attachment under the then-current configuration and store.
No semantic slice runs between a lane-1 commit and publication of its `derived_through`
or `Ready`, including on later reconcile, apply, recovery, rebuild and reload paths.
The existing unbounded inline post-leg drain must be replaced accordingly; slice size
and the concrete host scheduling carrier are implementation limits, not claims about
mechanisms already present. Work with a single slow model operation still obeys the
maintenance observation limit above, without converting the whole backlog into one turn.
Apart from the explicit delivery/initialization split, config, sidecar ownership, feed
epoch/generation, cancellation and teardown retain their lane-2 contracts.
Status and search answers keep the engine's actual availability and
watermarks. Semantic work neither writes correctness state nor blocks ordinary lexical
readiness merely because it has more feed to consume. A model-free host makes no semantic
availability claim, and scheduling tests do not establish real-model cost or quality.

An attachment that recreates its semantic engine can publish canonical `Ready` before
the vector rung becomes usable, including a warm reattachment after idle detach.
Status reports preparation during that interval; its duration is not promised here.

## From-zero build prices

Measured on a lab prototype in release builds, as medians of a serial matrix run in
Williams order, on a vault's first build over a 10,014-document fixture unless stated,
against a build serving early reads from ADR 0043's disposable sources. Each figure is end
to end, early reads served; ADR 0044's link-health figures come from a phase profile of
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
in document ids, in schema text and in full-text integrity.

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
