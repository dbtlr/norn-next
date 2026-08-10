---
status: accepted
date: 2026-08-10
---

# 0017 — watcher synchronization gates readiness

An entry may publish `Ready` only after it has both a hash-authoritative view of the
filesystem and proof that its watcher covers every registered edge. Attach and recovery use
one serial coverage-establishment protocol: install all coverage edges, wait for backend
synchronization, run the full heal, drain and reconcile batches accumulated during the heal,
then publish `Ready`. The synchronization boundary establishes when observation begins; the
heal establishes current truth; the final drain closes races during the heal.

Synchronization is watcher control state, not a filesystem fact. A subscription distinguishes
`Synchronizing`, `Live`, and `Terminal(error)`. A synchronization marker never becomes a
filesystem batch or a synthetic vault rescan. Only a backend signal that the observed path set
is incomplete can widen work to a vault rescan. Keeping these channels separate prevents a
benign vault-root event or readiness marker from hiding a coverage defect behind an expensive
full heal.

The synchronization barrier must not mutate the vault. Vaults are shared surfaces and can be
read-only, so a production canary would add an artificial document-side write and still leave
its meaning dependent on cleanup and permissions. Each backend instead proves its own coverage:
polling reports completion or failure of the initial baseline for every edge, and native macOS
watching reports completion of an event-history boundary that covers the complete plan,
including edges on different volumes.

Synchronization has a generous authored wall-clock deadline. Expiry is a typed terminal
watcher failure: the entry remains untrusted, and a later demand retries coverage establishment.
It is not watcher overflow, and one registration never switches silently from native watching
to polling. If the selected native backend cannot provide a sound barrier, registration
refuses with a typed error; polling remains an explicit operator selection. This preserves the
selected backend's cost and latency contract and keeps a broken native implementation visible.

The protocol is deliberately serial. Overlapping synchronization with the heal could reduce
cold-attach latency, but it would require generation and boundary machinery whose correctness
cost is not yet justified by measurement. A later decision can add that complexity if observed
attach cost warrants it.
