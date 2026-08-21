---
status: accepted
date: 2026-08-21
---

# 0021 — derived indexes split into two lanes, and inference never re-enters correctness

Every unit of derived state beyond the core document projection is an **index projection**:
a declaration naming its inputs — vault-document bytes, or named lane-1 pillars — whether it
is deterministic, and the one key that invalidates it wholesale. The lane is derived from
the declaration, never chosen. A deterministic projection of source bytes is **lane 1**:
written inside the atomic changeset, scoped to the changed set, and covered by the
convergence-to-equivalence bar. Everything else is **lane 2**: asynchronous, eventually
consistent, and maintained by an **engine** — a domain that owns a sidecar database and
derives from committed lane-1 records, never from vault files.

Lane 1 is structurally closed to everything else rather than policed, because its guarantees
all assume deterministic per-document locality. The changeset is the unit of atomicity and
is scoped to the blast radius; peak memory is a function of the working set; and incremental
maintenance must equal a from-zero rebuild over the same tree. Cross-document fan-out,
nondeterminism, and model-version dependence each break all three, so the projection that
carries any of them cannot ride the changeset at any price.

Lane 2 consumes change through a feed that is a query, not a structure: current document
rows and tombstones, ordered by the store's global write generation, projecting content
fingerprints so a consumer triages before it fetches. Cursors are consumer-owned and valid
within one store epoch; an epoch change means a rescan, and content-addressed sidecar rows
make a rescan recompute only what actually changed, so expensive derived state survives the
cheap state's rebuild. The host relays a contentless wake; a missed wake costs latency,
never correctness. A retained change log with registered subscriber cursors was rejected:
retention pinned by the slowest consumer forces an eviction policy, and eviction forces
consumers to resynchronize from current state — which is what the feed already is. Cross-file
transactional atomicity is not required, because eventual consistency is lane 2's stated
contract.

Inferred and higher-order derived state — any projection whose inputs include a model or
another index's output — is **query-surface-only**. It never becomes a finding, a plan, or a
repair input. This generalizes the boundary invariant that keeps `norn-embed` blind, and it
is carried the same way: absent crate edges, and a store read surface partitioned so the
correctness path has no API that exposes inferred pillars. A typed advisory finding class
for inferred results was considered and deferred: adding it later is additive, while
withdrawing inferred rows from the table the applier trusts would be nearly impossible.

Every projection names one invalidation key, and the key lives with the state it
invalidates: the vault schema fingerprint for lane-1 schema-keyed tables, an engine's own
configuration and model versions for its sidecar. How invalidated state converges is the
owning side's judgment. Wholesale rebuild is the current implementation and the permanent
always-correct floor; finer responses — adding one index without a rebuild, a pillar
carrying its own version key — are carved evolutions taken when something like measured cost
forces them, not obligations.
