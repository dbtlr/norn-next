---
status: accepted
date: 2026-08-09
---

# 0015 — wire reads run on store-minted snapshot readers, and teardown never waits for one

The store's one writer connection sits behind `&mut`, inside the attachment that every
lifecycle job holds for its whole duration — so a wire read borrowing it would serialize
behind every warm job that holds the store while trust stays `Ready` (polls, maintenance,
plan application), and the timing-barred query shapes would measure orchestration where the
acceptance contract bars SQL. Heals are not the motivating case: the first heal runs before
any reader exists, and a re-heal drops trust out of `Ready`, where reads already refuse. A
shared borrow of the live store cannot serve reads either — a `&self` cannot exist while a
job holds the attachment mutably. Wire reads
therefore run on dedicated read-only handles `norn-store` mints from a live `Store` —
opened read-only with `query_only` set, carrying the read builders and their `EXPLAIN` seam
so the gates exercise the connection reads actually use, and keeping the
no-connections-outside-store rule whole. The host holds the handle in entry state **beside
the attachment, never inside it**. This decision supersedes [0014](0014-snapshot-readers.md),
which recorded the same reader and priced the same costs; what it corrects is the reach of
the lifetime rule.

Each read is one WAL snapshot transaction, **established under the entry gate lock in the
same critical section that reads trust** — established by a read statement, because a bare
deferred `BEGIN` takes no snapshot — so the trust label and the snapshot describe the same
instant; the read itself runs outside the lock, at the priced cost of the hot per-entry
lock riding across that one statement — a cheap read alone, a wait where a concurrent read
still holds the one reader. A snapshot sees the last committed
increment, never a torn one, never blocks the writer, and may trail in-flight derivation;
trust state, not the connection, is what buys the right to answer. On the one reader,
concurrent reads serialize against each other, and that measured contention — not an
assumption — is what mints more readers through the same seam: the pool is the carved
extension point, not the starting shape. A reader never concludes a
heal rung: its open bypasses inspection and rebuild, and `query_only` makes derivation
impossible by construction, which turns the warm-read zero-derivation-counter bar
structural.

The lifetime rule is carried by one seam, never by per-site discipline. The reader is
minted where the coverage it reads is installed, as one move under the lock that publishes
the trust label beside them, and it goes back before the store it was minted from closes —
at the window every teardown enters, with the identity-refusal route carrying the rule
around that window. A read in flight shares the entry's one handle rather than taking it
out, so the teardown that empties the slot closes no handle a read is running on, and no
new read begins once the slot is empty. The pin a read takes states one narrow fact — work
running outside the entry's lock is coming back — and what it buys the read is deferral
alone: **while it stands, no idle detach is scheduled against the entry. Teardown never
waits for a read.** A
refusal or a destruction moves first and consults no pin; an idle detach already scheduled
when the read began tears the entry down under it; a job leg failing its way into a
release reads no pin either. Through any of them the read keeps answering from the handle
it holds and from nothing else — no move states that the file behind that handle outlives
the teardown, and nothing pins one that would; the obligation is deliberately left
unstated until the read path lands and prices it.

The alternative was the pin as a teardown veto — an entry that cannot be torn down while
any read is in flight, which is what the superseded record asserted. It loses on what a
read holds: nothing. What defers the give-back at a refusal or a destruction is coverage
custody, not the pin — the leg is out with the entry's coverage and the give-back is the
leg's own end; a read holds no coverage, so at those sites the entry is still holding its
own and the refusal or the destruction is itself what reaches detach, under the live hold
rather than at a leg's end. Granting a read that deferral would put reader count in front
of exactly the moves that must make progress — a conflict being refused, a host coming
down, a maintainership already lost — while buying nothing the shared handle does not
already carry.

The price of the second connection per attached entry is named: three descriptors, a
second page cache and prepared-statement cache under the gated memory ceiling, and a held
snapshot pinning the write-ahead log against checkpointing — checkpointing stays passive,
which is what keeps the reader's never-blocks-the-writer guarantee whole, and the bounded
read shapes are what keep the WAL pin short. The corrected rule prices one more: a read that a
throwaway teardown runs under finishes against files the teardown has unlinked, and its
descriptors ride until the hold drops.
