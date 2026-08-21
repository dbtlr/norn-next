---
status: accepted
date: 2026-08-21
---

# 0022 — one crate knows SQL, and every database is a domain over it

`norn-db` owns the mechanics of running a SQLite database: connection ownership, DDL
fingerprinting and the pinned-scalar meta pattern, changeset transaction discipline,
damage typing at the driver seam, the `EXPLAIN` plan handout, database file lifecycle, and
epochs. No other crate opens a SQLite connection. `norn-store` is `norn-db`'s first client
and owns what the main derived database means — the lane-1 DDL, the request semantics, and
the read builders. A lane-2 engine's sidecar database is the second client.

The boundary was already named before it was real: `norn-store`'s membership rule is "an
SDK for talking to SQL." With one client ever, the SDK and its first client grew into one
body. A second database consumer exposes the fusion's cost in both directions: either every
engine re-grows rebuild, fingerprint, and damage machinery for its own sidecar, or the
lane-1 crate learns the mechanics of lanes it should not know exist.

What stays together is as deliberate as what splits. DDL and read builders co-evolve in the
domain crate, because the `EXPLAIN` gates bind builder-emitted SQL to the schema it reads —
the co-evolution argument binds builder to schema, not builder to connection. The plan
handout — a plan cannot be taken by a crate that may not open a connection — already serves
a connection-less consumer and becomes the domain crates' interface to plans. The
connection lint's subject narrows to a crate with no domain content in it.

Extraction is pulled, never pushed. `norn-db`'s API is what its second consumer actually
demands; everything the sidecar does not touch stays in `norn-store` until a need moves it.
A symmetric up-front generalization was rejected as the known failure mode of abstractions
extracted against a single consumer's shape.
