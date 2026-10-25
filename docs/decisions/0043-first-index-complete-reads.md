---
status: accepted
date: 2026-10-25
---

# 0043 — First indexing may serve a request whose complete answer is already trustworthy

Supersedes [ADR 0042](0042-elastic-readers-and-concurrent-retained-results.md).
The elastic reader budget, canonical read acquisition, demand lease, recovery
restraint, settling bound, teardown, and retained-count rules are restated below.
The change permits complete Get and supported metadata Find during first indexing,
under request-specific admission. Whole-vault readiness retains its existing meaning.

## Context

A complete requested document can be derived before every document in a vault.
Complete metadata can answer field and path queries before full rich derivation.
Making every query wait for global readiness couples simple reads to unrelated
construction. Answering from arbitrary partial rows instead would give false
absence, incomplete content, or incorrect resolution.

The chosen trade-off is bounded additional temporary construction and lifetime
management in exchange for useful earlier answers. The same query builders and
full-result requirements apply. Availability depends on a proven complete request,
not on a percentage of files processed or an optimistic readiness label.

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
acceptance contract bars SQL. Canonical acquisition is unavailable until the first heal
and drain publish `Ready`; a canonical read meeting a never-served entry refuses at once.
Request-specific first-index capabilities may answer earlier under the rules below.
Every later heal takes trust out of `Ready`. A read waits out the heals that take in a
change — a watcher batch's reconcile and a vault schema reload's — and refuses at once
during the heals that follow withdrawn trust — a recovery's and a rebuild's. A shared
borrow of the live store cannot serve reads either — a `&self` cannot exist while a job
holds the attachment mutably. Wire reads therefore run on pooled read-only handles
`norn-store` mints from a live `Store` or sealed, owned first-index construction.
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
canonical stances. First-index Get and metadata Find additionally use the request-specific
admission below; they do not turn a never-served entry into a canonical settling entry.

- **Settle.** An entry that entered warming from `Ready`, with no withdrawal of trust in
  between, settles, and a read that meets it waits. That is an entry taking in a change: a
  polled watcher batch, a reconcile turn, or a vault schema reload.
- **Refuse.** Everything else refuses at once, with its reason: an entry that has never
  served unless this request earns first-index admission below, an entry that lost trust —
  a watcher overflow or loss, an environmental refusal,
  damaged derived state — a release, a detach or a drop, and every park. Warming entered
  from untrusted, which is a recovery's or a rebuild's, refuses at once, although it too runs
  over coverage that was once `Ready`.

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

## Complete requests during first indexing

### Admission is specific to the requested operation

A registered vault undergoing its first index construction may answer a default whole
Get or supported field/path Find before global `Ready`. This permission belongs to the
current initial-attach claim and its synchronized watcher coverage. It does not admit
reads during a recovery, rebuild, schema reload, park, or release that otherwise refuses.
An existing canonical index keeps its ordinary attachment and read path.
Only a newly created, initially empty canonical store qualifies. Reuse of an existing
store and rebuilding an existing store inside the attach leg do not qualify. A later
inline rebuild closes first-index admission; it cannot remint the earlier authority.

A complete names catalogue precedes every early answer. Whole Get additionally requires
complete derivation of the resolution candidates for the requested resolution target,
including its ambiguity class and bounded naming/link context. That context includes
linked document candidates' headings and blocks needed for ordinary link health. The
ordinary body, fields, findings and resolved links are complete. Ambiguity keeps its
ordinary judgment. An incomplete catalogue or exceeded construction bound is
never a successful missing-document result. Complete metadata precedes field/path Find;
only predicates, projections and ordering its existing guarded builders support are
eligible. Missing or unsupported completeness stays on the ordinary loading path.

Targets come from actual requests, including requests arriving during construction.
No target is configured before attachment merely to qualify for early service. Demand
is owned by the request's existing lease. Pending interests, retained publications and
total additional target construction have explicit bounds independent of vault size.
Expired interest releases its reservation. Distinct incoming targets cannot indefinitely
delay canonical construction; excess or unprepared requests receive ordinary loading
behavior. A sealed catalogue may stay on disk while bounded construction checkpoints
service admitted targets. There is no vault-sized in-memory target map.

Get and Find use one request-aware acquisition algorithm with canonical acquisition.
The ordinary public unrestricted read hold remains canonical. Early ownership exposes
only the operation for which completeness was established; it lends no unrestricted
snapshot, reader, count, rich-query, plan or mutation capability. Actual execution checks
the request shape again and invokes the ordinary builder. There is no second query
language, filter, parser, resolver, or client API.

A supported first-open request may wait for its capability within the original read
acquisition deadline. The first gate take, construction-availability wait, capacity/open
wait, source replacement and gate retakes share that deadline. No phase renews it.
Unsupported shapes and operations retain their existing loading behavior. Publication,
withdrawal and closure of target admission wake affected waiters; progress counters
alone are not an availability signal. No wait runs under the entry gate.
Expiry while waiting for an early capability returns `host/entry-not-ready` with the
current warming phase and counters and the message "this vault is still indexing for
the first time". Resource and gate-acquisition failures keep their existing refusal
rules. This adds a price: an eligible first-open caller can be held up to the ordinary
bound, whereas a canonical first-open request still refuses at once.

### Source authority and publication

The initial catalogue and early construction are covered by one watcher authority
captured before enumeration. The first implementation uses conservative withdrawal:
any accepted relevant filesystem fact or observed source contradiction invalidates that
authority. It cannot be restored by draining a batch, finishing a heal window or seeing a
missing root return. Lost or overflowed coverage prevents early success.

The authority includes every relevant fact the subscription accepts after capture,
wherever it is held: the coalescer's pending or unplaced batch, an open heal window,
collected heal observations and the delivery slot are examples. The entry's merged
`pending.taken_in` counter
alone is not this boundary. A request fixes its requirement when it meets the entry;
known facts cannot be omitted merely because the attach leg has not returned them yet.
A withdrawn early capability yields to ordinary reconciliation rather than repeatedly
moving the request's requirement until the filesystem falls quiet.

Publication binds source identity, supported operation, pinned content model, watcher
authority and attach claim atomically under the entry gate. A changed claim, registration,
root, schema, known damage, park or release prevents stale publication. The published
source is separate from the canonical pool and does not replace the canonical model or
plan ground. It never sets `trust_unbroken_since_ready` or global `Ready`.

A bounded root-identity check is permitted on this early acquisition path, outside the
entry gate and watcher locks. It reads no document and runs no derivation. The canonical
warm request path keeps its existing filesystem-check-free behavior. This is a scoped
exception to the architecture's request-path filesystem-check prohibition and the
"never per request" alternative below; it does not permit other filesystem query work.
Bookkeeping-only claim, authority, source and deadline checks run under the establishing gate hold;
filesystem work, opening, waiting and cleanup do not. Watcher callbacks never take the
entry gate, including when they invalidate authority or wake a waiter.
The lock order is entry gate before watcher bookkeeping, never the reverse.

After a cold open or capacity wait, acquisition rechecks the current claim, publication,
authority, request shape and pinned model under the original deadline. A replaced source
returns its grant and reselects. Before exposing an early result, the read checks that no
known source or coverage contradiction withdrew its authority. Store, resource and
environmental failures keep their types even when eligibility also moves.
Every source-dependent refusal is an early result for this authority check: unknown or
ambiguous target, cursor position/order refusals and other judgments derived from indexed
data cannot bypass it by being represented as an error. Source-independent input errors
retain their ordinary validation. This is a watcher-fidelity guarantee, not filesystem
atomicity or proof that an unseen operating-system event did not occur.

### Lifetime, budget and promotion

Prepared sources remain available while later initial construction runs, so availability
is not limited to a construction callback. Their writers close before immutable public
readers use them. Store-owned temporary files outlive every established read that owns
them. Failed and abandoned construction retains normal cleanup and interruption handling.
These files live beside the canonical derived store, outside the vault and its shadow
home. Recognized orphan files and their sidecars are swept only under the corresponding
maintainer lock; a sweep never deletes storage held by another maintainer.
A complete rich Get projection must retain rich derivation and ordinary link health;
metadata's payload-omitting construction is not a complete Get source.

Early pools use the same host-wide reader budget and the same registration fairness group
as the canonical pool. Physical temporary-file names do not buy extra fair turns. Source
identity remains distinct for opening, epoch provenance and damage judgment. Opening,
active, idle and retired-but-held resources remain charged until actual closure. Supported
Get and Find acquire source capacity, not retained-count scratch capacity.

Promotion closes new early acquisitions in the gate hold that publishes canonical
`Ready` with its reader. If accumulated facts require reconciliation first, their
acceptance has already withdrawn conservative early authority. Healthy promotion and
administrative retirement do not invalidate
an already established immutable read; its held proof and resources survive independently
of new admission. Explicit detach, unregister and host destruction count as administrative
retirement, including the unsubscription they cause. Established authority must distinguish
that administrative end from backend failure, overflow or root/source loss. Dropping the
subscription alone does not erase the proof a held read already earned or permit new
acquisition. Known source or coverage contradiction is different and withdraws
the early result, including a source-dependent refusal. Teardown never waits for a read,
a cold open or physical
last-owner cleanup. Idle owners and replaced turns close outside the gate.

Temporary-storage cleanup failures remain observable after the final owner releases;
a legitimate held read is not itself a cleanup failure. Late errors carry the physical
source identity and role. Damage to a disposable source withdraws that source and refuses
its read; it is not evidence that a different canonical database is corrupt. Canonical
source damage retains the existing claim-bound rebuild path. Environmental failures do
not become damage rebuilds, and no final publication may erase a previously observed
operation or cleanup failure.

An early answer carries the actual warming trust, source epoch and committed generation
of its establishing hold. A later canonical answer carries the canonical source identity.
Find continuations use ordinary cursor judgment: compatible epoch movement continues with
movement reported; incompatible order/schema retains the ordinary refusal. No cursor is
rewritten, silently restarted or implicitly promoted into a retained-count result.

### Whole-vault and semantic readiness

Global lane-1 `Ready` still requires canonical construction, watcher synchronization and
reconciliation of the accumulated facts. Request-specific early permission does not weaken
that boundary. Canonical lexical/rich queries become useful at that boundary even while
optional semantic initialization or feed consumption remains outstanding.

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

### Costs and alternatives

This choice adds bounded catalogue/projection ownership, request interests and temporary
construction before canonical completion. It can increase total startup work and delay
whole-vault readiness even while complete early answers arrive sooner. Published sources
and their metadata are retired after promotion; the canonical warm path does not retain a
vault-sized catalogue or a history of initial requests. Exact result and final-index
correctness remain requirements. Operation-by-operation timing differences remain visible
rather than being combined into an aggregate offset.
An attachment that recreates its semantic engine can publish canonical `Ready` before
the vector rung becomes usable, including a warm reattachment after idle detach.
Status reports preparation during that interval; its duration is not promised here.

Making the whole partial store publicly readable would make completeness depend on which
rows happened to arrive. Serving directly from files would duplicate query, resolution
and lifetime rules. Calling a partial store `Ready` would hide unfinished canonical work.
Those alternatives are rejected. The chosen capability restriction buys earlier complete
answers while retaining the existing ordinary query meanings.

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
