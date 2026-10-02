---
status: accepted
date: 2026-10-02
---

# 0033 — every vault operation requires a registration

Every request that reads or writes a vault names a registered vault. There is no
unregistered mode: no disposable derivation over a throwaway store, and no request addressed
to a vault by its root path. A directory no registered vault contains is answered by that
absence — `vault resolve` finds no vault for it — and a surface meeting one refuses and
names the command that sets it up: `init`, which registers the directory and scaffolds its
schema. Registration is what gives a vault its name, its derived-state directory, its
maintainer lock and its shadow home, and the attach, the serialization of its applies and
the in-vault shadow fallback's placement are all keyed by them, so one path serves every
operation.

The alternative was the design the serving layer was drawn with: registration gates
durability, and an unregistered root gets disposable derivation over a throwaway store torn
down after the request. Its host half was never built: the store opens a throwaway database,
but the attach seam refuses a throwaway demand, and the root address is carried dormant for
Layer 6 (the surface layer). Building it would add a second attach mode with its own
teardown, a lock keyed by root identity because no registration serializes the root's
applies, and a fallback placement computed for a maintainership that never persists. Its
uses were a one-off query over a directory never set up and a job over a fresh checkout.
Registering first serves both for one more command, and a throwaway derivation pays the same
derivation from zero a registration's first attach pays.

This withdraws the roadmap obligation the root address and the throwaway attach mode carried
as dormant carriers for Layer 6, so both may be removed. The store's throwaway mode stays as
an ephemeral store for tests and loses its unregistered-root rationale. Earlier ADRs that
name a throwaway teardown among the paths that close a store, the prices of [ADR
0030](0030-a-read-does-not-restart-a-recovery-only-a-change-can-answer.md) among them,
describe a path that will not exist; their rules hold over the paths that remain.
