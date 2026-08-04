---
status: accepted
date: 2026-08-04
---

# 0014 — wire reads run on store-minted snapshot readers

The store's one writer connection sits behind `&mut`, inside the attachment that every
lifecycle job holds for its whole duration — so a wire read borrowing it would serialize
behind every warm job that holds the store while trust stays `Ready` (polls, maintenance,
plan application) and behind every other read, and the timing-barred query shapes would
measure orchestration and queueing where the acceptance contract bars SQL. Heals are not
the motivating case: the first heal runs before any reader exists, and a re-heal drops
trust out of `Ready`, where reads already refuse. Wire reads therefore run on dedicated
read-only handles `norn-store` mints from a live `Store` — opened read-only with
`query_only` set, carrying the read builders and their `EXPLAIN` seam so the gates exercise
the connection reads actually use, and keeping the no-connections-outside-store rule whole.
The host holds the handle in entry state **beside the attachment, never inside it**. The
one alternative the store module had penciled in — reads as a shared borrow of the live
store — loses on exactly that coupling: a `&self` cannot exist while a job holds the
attachment mutably, so the module contract is superseded on that point by this decision.

Each read is one WAL snapshot transaction, **established under the entry gate lock in the
same critical section that reads trust** — the trust label and the snapshot describe the
same instant — and run outside the lock. A snapshot sees the last committed increment,
never a torn one, never blocks the writer, and may trail in-flight derivation; trust
state, not the connection, is what buys the right to answer. On the one reader, concurrent
reads serialize against each other, and that measured contention — not an assumption — is
what mints more readers through the same seam: the pool is the carved extension point, not
the starting shape. A reader never concludes a heal rung: its open bypasses inspection and
rebuild, and `query_only` makes derivation impossible by construction, which turns the
warm-read zero-derivation-counter bar structural.

The lifetime rule is carried by one seam, never by per-site discipline: the lifecycle
mints the reader where it publishes the attachment and tears it down before the store
closes on every closing path — detach, throwaway teardown (which unlinks the database,
`-wal`, and `-shm`), and any in-life discard — and an in-flight read pins the entry the
way jobs do, so no teardown unlinks a file a reader still holds and no reader survives to
serve snapshots of a discarded database. The price of the second connection per attached
entry is named: three descriptors, a second page cache and prepared-statement cache under
the gated memory ceiling, and a held snapshot pinning the write-ahead log against
checkpointing — the bounded read shapes are what keep that pin short.
