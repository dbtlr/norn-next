---
status: accepted
date: 2026-07-31
---

# 0011 — shadows live outside the vault

A write's shadow is staged under the vault's norn data directory, falling back to `<vault>/.norn/tmp` only when the vault sits on a different filesystem, and never as a sibling of its destination. The vault tree belongs to every consumer — snapshot automation, sync clients, editors — and mechanism files placed among documents get committed, synced, and indexed by tools that cannot know to ignore them. The atomic swap is a rename, which cannot cross filesystems, so placement is decided per vault at attach by device comparison and recorded as a typed fact; a fixed global temp location is structurally impossible. The fallback directory sits outside the vault-document boundary: the walk excludes it wholesale and leaked shadows in it are swept. A shadow's invisibility means it is never surfaced as a document — not confidentiality, which Norn does not claim; a shadow is as readable as the vault it will join.
