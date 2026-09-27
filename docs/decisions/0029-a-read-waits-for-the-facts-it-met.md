---
status: accepted
date: 2026-09-26
---

# 0029 — wire reads run on store-minted snapshot readers, a read's hold is demand, a read waits for the facts it met and never for the vault to fall quiet, and teardown never waits for one

Supersedes [ADR 0028](0028-a-read-waits-out-a-change-it-was-served-over.md), whose reader,
lifetime rule, demand lease, read stance, bound and prices this decision restates whole.
ADR 0028 ruled that a settling read establishes its snapshot only where the published
reading is `Ready`, that it answers from the first `Ready` it observes, and that it does not
wait for the vault to fall quiet. Under steady edits those three cannot all hold. A
reconcile turn that drained new facts hands on to the next turn and publishes no `Ready`
between the two, so a vault edited faster than one turn takes publishes no `Ready` at all,
and every read that meets it refuses at its bound: a reproduction ran 66,619 reconcile
turns inside one read's 300 ms wait. **A settling read records how far the facts the entry
had taken in ran when the read met it, and answers once the entry is `Ready` or has derived
every one of those facts with a reader standing. Its snapshot is never older than the change
it arrived during, and it never waits for the vault to fall quiet.** The invariant ADR 0028
bought — no read answers from the state before the change it met — is kept; what changes is
that the read no longer waits for a `Ready` a busy vault never publishes.

Everything else ADR 0028 ruled stands, including what it kept from ADR 0025: a read's hold
is demand on the entry, and it is the lifecycle's own demand lease that says so; the
reader's open is a **fallible mint** that answers a reason rather than an absence and never
panics under the entry gate; **no acquisition waits for the entry's connection while it
holds the entry gate**, a rule the read's wait obeys in the same way; what settles and what
refuses at once; the bound; teardown waking a waiting read; and the known limit during a
maintenance scan. ADR 0028 itself superseded [0025](0025-a-reads-hold-is-demand.md), which
superseded [0015](0015-snapshot-reader-lifetime.md), which superseded
[0014](0014-snapshot-readers.md); what 0015 corrected was the reach of the lifetime rule,
what 0025 corrected was what a read's hold is, what 0028 corrected was what a read does when
the vault it reads is changing, and what this record corrects is when that read may answer.

The store's one writer connection sits behind `&mut`, inside the attachment that every
lifecycle job holds for its whole duration — so a wire read borrowing it would serialize
behind every warm job that holds the store while trust stays `Ready` (polls, maintenance,
plan application), and the timing-barred query shapes would measure orchestration where the
acceptance contract bars SQL. Heals are not the motivating case: the first heal runs before
any reader exists, and every later heal takes trust out of `Ready`. Under the stance ruled
below, a read waits out the heals that take in a change — a watcher batch's reconcile and a
schema reload's — and refuses at once during the heals that follow withdrawn trust — a
recovery's and a rebuild's. A shared borrow of the live store cannot
serve reads either — a `&self` cannot exist while a job holds the attachment mutably. Wire
reads therefore run on dedicated read-only handles `norn-store` mints from a live `Store` —
opened read-only with `query_only` set, carrying the read builders and their `EXPLAIN` seam
so the gates exercise the connection reads actually use, and keeping the
no-connections-outside-store rule whole. The host holds the handle in entry state **beside
the attachment, never inside it**.

Each read is one WAL snapshot transaction, **established under the entry gate lock in the
same critical section that reads trust** — established by a read statement, because a bare
deferred `BEGIN` takes no snapshot — so the trust label and the snapshot describe the same
instant; the read itself runs outside the lock. **No acquisition waits for the entry's
connection while it holds that lock.** Under the gate it tries for the connection without
blocking and establishes the snapshot there where it is free; where another read holds the
connection, the acquisition gives the gate back, waits outside it under the demand it has
already recorded, takes the gate again and reads the published demand afresh before it
establishes. Where the entry has stopped serving, or its reader is no longer the one waited
for, the acquisition gives the connection back and answers what the entry now publishes
under the stance ruled below: where the entry settles without answering the read, it waits;
where the entry answers the read on a reader other than the one it waited for, it acquires
that reader, within the same bound; and it refuses with what the entry publishes only where
the entry neither settles nor serves — lost trust, a release, a park. **The wait for the
connection ends at the read's bound**: a read that finds the connection still held past it
refuses as reader-unavailable, since the entry serves and only its read seam is taken.

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
connection, is what buys the right to answer. On the one reader, concurrent reads serialize
against each other, and that measured contention — not an assumption — is what mints more
readers through the same seam: the pool is the carved extension point, not the starting
shape. A reader never concludes a heal rung: its open bypasses inspection and rebuild, and
`query_only` makes derivation impossible by construction, which turns the warm-read
zero-derivation-counter bar structural.

The lifetime rule is carried by one seam, never by per-site discipline. The reader is
minted where the coverage it reads is installed, as one move under the lock that publishes
the trust label beside them — a fallible mint that cannot panic, since an unwind under the
entry gate poisons it, and whose blocking open is a priced cost of that hold — and it goes
back before the store it was minted from closes, at the window every teardown enters,
with the identity-refusal route carrying the rule around that window. A read in flight
shares the entry's one handle rather than taking it out, so the teardown that empties the
slot closes no handle a read is running on, and no new read begins once the slot is empty.

**A read's hold is a demand lease, taken under the same hold of the entry gate that reads
the published demand and clones the handle.** What the lease buys is what it buys every
other caller: it holds the entry's idle interval open for as long as the read runs and
restarts it when the hold drops, it clears the idle deadline, it withdraws an idle detach
that is scheduled and not yet in flight, it raises the recovery the entry owes and gives
that demand back with the hold, and where the entry is free to run it, it schedules the
work the entry owes, read as a chain: the attach where the entry holds no coverage, and
under that the rebuild it owes, the recovery beneath that, and the reconcile where it owes
neither — with the read answering under what that work publishes, and under the state it
found where the work publishes none. A read asks for the owed recovery because what a read wants from an
untrusted vault is exactly that it become answerable again; a read workload that never
healed the vault it reads would be a dead end. So an idle teardown neither runs under a
read nor precedes one into the entry, and a workload of reads alone keeps an attached vault
attached and an untrusted one converging. **Teardown never waits for a read**, and the
lease changes nothing about that: a refusal or a destruction moves first and consults no read, and a job leg failing its way
into a release consults none either. Through any of them the read keeps answering from the
handle it holds and from nothing else — no move states that the file behind that handle
outlives the teardown, and nothing pins one that would. The read path states that absence
rather than leaving it open: a read that a teardown runs under is answering against files
the teardown may already have unlinked. The pin stands beside the lease and keeps its own
narrow job, which is to say that the entry is held by work outside its lock.

**A read's demand differs from every other demand in exactly one step: it withdraws no
park.** An ordinary demand retires the registry's parks — a duplicate root, a root the
registry could not read — where maintainer contention is not already answering it, because
withdrawing them is how a caller asks for the acquisition that classifies those roots
again; a contended entry keeps every park it stands on, since no acquisition follows a
demand the contention answers. A read asks for no such acquisition under any condition; it
asks for an answer, and a vault the registry has parked has none to give. A read against a parked
entry therefore refuses with the park's own code — identity refused, duplicate root and
maintainer contention alike — and schedules nothing.

## A read waits for the facts it met

**Whether a read waits is its stance, and the stance is read under the entry gate, in the
same critical section that reads the published demand.** There are two stances.

- **Settle.** An entry that entered warming from `Ready`, with no withdrawal of trust in
  between, settles, and a read that meets it waits. That is an entry taking in a change: a
  polled watcher batch, a reconcile turn, or a schema reload.
- **Refuse.** Everything else refuses at once, with its reason: an entry that has never
  served, an entry that lost trust — a watcher overflow or loss, an environmental refusal,
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
during**. A schema reload waits for `Ready`, because it replaces the reader: a read it
settles answers from the reader minted once `Ready` returns. **A settling read that finds a
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
takes the gate again and refuses with what the entry then publishes.

**The bound is operational containment, not a performance threshold.** It keeps a read from
waiting on a change that does not converge, and it states nothing about how fast a change
should settle. It is a lifecycle policy value, and its production value equals the settle
ceiling already authored for one vault walk: 5 seconds. The bound runs from the read's
first hold of the entry gate and covers every wait the read takes, the wait for the
connection and the wait for the change together.

### Drivers

- **Least surprise.** A client that edits a note and then queries it sees the edit, even
  while other notes are being edited.
- **Trust over speed.** A read never answers from a state it knows is stale.
- **No retry burden on every client.** Without the wait, every client, agents included,
  must catch a refusal and retry after each edit it makes.

### Known limit

While a background maintenance scan holds the entry, the entry stays `Ready` and does not yet
see an edit made moments before, so a read in that window can miss that edit; this is
accepted as rare.

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

## Prices

The price of the second connection per attached entry is named: three descriptors, a second
page cache and prepared-statement cache under the gated memory ceiling, and a held snapshot
pinning the write-ahead log against checkpointing — checkpointing stays passive, which is
what keeps the reader's never-blocks-the-writer guarantee whole, and the bounded read shapes
are what keep the WAL pin short. The lifetime rule prices one more: a read that a throwaway
teardown runs under finishes against files the teardown has unlinked, and its descriptors
ride until the hold drops. The lease prices another: a vault under continuous read
traffic is a vault the idle budget stops bounding, which is what the idle interval is for, and the
lease's restart on drop is what keeps that honest. The wait prices another: a read that
meets a change holds its caller for up to the bound, and its demand holds the entry's idle
interval open for as long as it waits. Answering before `Ready` prices the last one: an
answer can be taken while later facts are still being derived; it says so in the trust it
carries, and a client that needs those later facts must read that trust rather than assume
`Ready`.
