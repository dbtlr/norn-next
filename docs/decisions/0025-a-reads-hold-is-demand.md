---
status: accepted
date: 2026-09-20
---

# 0025 — wire reads run on store-minted snapshot readers, a read's hold is demand, and teardown never waits for one

Supersedes [ADR 0015](0015-snapshot-reader-lifetime.md), whose reader, lifetime rule and
prices this decision restates whole. ADR 0015 gave a read a pin, and a pin states one narrow
fact: work running outside the entry's lock is coming back. Read where a detach is
scheduled and nowhere after, it left a read that began behind a scheduled detach to be torn
down under its own live hold, and it left a workload of reads alone with no way back to an
attached entry, because a pin is not demand. **A read's hold is demand on the entry, and it
is the lifecycle's own demand lease that says so.** One further ruling arrives here that
ADR 0015 does not carry: the reader's open is a **fallible mint** that answers a reason
rather than an absence, and that never panics under the entry gate. ADR 0015 itself
superseded [0014](0014-snapshot-readers.md), which recorded the same reader and priced the
same costs; what 0015 corrected was the reach of the lifetime rule, and what this record
corrects is what a read's hold is.

The store's one writer connection sits behind `&mut`, inside the attachment that every
lifecycle job holds for its whole duration — so a wire read borrowing it would serialize
behind every warm job that holds the store while trust stays `Ready` (polls, maintenance,
plan application), and the timing-barred query shapes would measure orchestration where the
acceptance contract bars SQL. Heals are not the motivating case: the first heal runs before
any reader exists, and a re-heal drops trust out of `Ready`, where reads already refuse. A
shared borrow of the live store cannot serve reads either — a `&self` cannot exist while a
job holds the attachment mutably. Wire reads therefore run on dedicated read-only handles
`norn-store` mints from a live `Store` — opened read-only with `query_only` set, carrying
the read builders and their `EXPLAIN` seam so the gates exercise the connection reads
actually use, and keeping the no-connections-outside-store rule whole. The host holds the
handle in entry state **beside the attachment, never inside it**.

Each read is one WAL snapshot transaction, **established under the entry gate lock in the
same critical section that reads trust** — established by a read statement, because a bare
deferred `BEGIN` takes no snapshot — so the trust label and the snapshot describe the same
instant; the read itself runs outside the lock, at the priced cost of the hot per-entry
lock riding across that one statement — a cheap read alone, a wait where a concurrent read
still holds the one reader. A snapshot sees the last committed increment, never a torn one,
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

The price of the second connection per attached entry is named: three descriptors, a second
page cache and prepared-statement cache under the gated memory ceiling, and a held snapshot
pinning the write-ahead log against checkpointing — checkpointing stays passive, which is
what keeps the reader's never-blocks-the-writer guarantee whole, and the bounded read shapes
are what keep the WAL pin short. The lifetime rule prices one more: a read that a throwaway
teardown runs under finishes against files the teardown has unlinked, and its descriptors
ride until the hold drops. The lease prices the last one: a vault under continuous read
traffic is a vault the idle budget stops bounding, which is what the idle interval is for, and the
lease's restart on drop is what keeps that honest.
