---
status: accepted
date: 2026-07-28
---

# 0002 — fixture identity is build-scoped

Within a given build, `(profile, seed)` identifies the paths and bytes the fixture generator emits; changing the generator may change that tree. Identity is defined over emission rather than filesystem readback because filename normalization can change a path's byte spelling across machines.
