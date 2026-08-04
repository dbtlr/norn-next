---
status: accepted
date: 2026-08-04
---

# 0014 — wire reads run on store-minted snapshot readers

The store's one writer connection lives inside the attachment, and every lifecycle job holds
the attachment for its whole duration — so a wire read that borrowed that connection would
starve for the length of a full tree heal and serialize behind every scoped increment,
measuring orchestration contention where the acceptance contract bars SQL. Wire reads
therefore reach the database through dedicated read-only handles that `norn-store` mints
from a live `Store` — opened read-only with `query_only` set, carrying the read builders,
derivation counters, and their own prepared-statement cache, so the `EXPLAIN` and counter
gates exercise the connection reads actually use and the no-connections-outside-store rule
stays whole. The host places the handle in entry state **beside the attachment, never inside
it**: a read locks the entry gate only to check trust and take the handle, then runs outside
the lock while any lifecycle job holds the store.

Each read is one WAL snapshot transaction: it sees the last committed increment, never a
torn one, never blocks the writer, and may trail in-flight derivation by design — trust
state, not the connection, is what buys the right to answer. A reader is minted from a live
`Store` and dies with the attachment that owns it; a rung-3 rebuild replaces the store, so
a fresh attachment mints a fresh reader and no handle survives to serve snapshots of a
discarded database file. One reader per entry is the starting shape, its descriptor counted
against the measured attach budget; the minting seam is the carved extension point for a
pool, taken only when a measured bar demands it — an always-on pool would spend descriptors
against that budget on an assumption.
