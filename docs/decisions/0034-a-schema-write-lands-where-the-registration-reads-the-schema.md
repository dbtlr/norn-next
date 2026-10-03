---
status: accepted
date: 2026-10-03
---

# 0034 — a schema write lands where the registration reads the schema

A `write_control_file` of the schema lands at the file the vault's registration reads its
schema from: the default `.norn/schema.yaml`, a `schema_source` inside the vault root, or a
`schema_source` outside it. The transition keeps naming the role's canonical path,
`.norn/schema.yaml`, and the planner and the applier resolve that role against the
registration each time they touch it: at planning, when they observe the target, at staging
and at publication. Before this ruling every vault write was rooted at the vault root, and a
schema write under a registration naming a `schema_source` did not resolve. A write outside
the root is the one exception to that rule, and the schema is the only file it covers.

An out-of-vault write is anchored at the source's parent folder. The folder's identity is
recorded at staging and checked again at publication, the guard the vault root gets for
every other target, so a folder replaced between the two phases publishes nothing. The
shadow stages in the vault's existing shadow home: [ADR
0011](0011-shadows-live-outside-the-vault.md) stands whole, and no shadow is placed beside
the source. A publication is a rename, which cannot cross filesystems, so a write whose
shadow home cannot be renamed into the source's folder is refused, naming why, before
anything is staged. That covers a home on another filesystem than the folder, and the
in-vault fallback home, which the write kernel reaches only through the vault root. There is
no copy fallback, for the reason ADR 0011 gives: it would publish bytes without a rename.

`vault migrate` needs this. Its job is to bring the schema the vault actually reads up to the
version this build reads, and for a registration naming a `schema_source` that file is the
source. Registration refuses a schema file another registration already uses, compared by
file identity, so a schema write for one vault never rewrites another's. `init` keeps
answering `schema_elsewhere` and never writes at a source: scaffolding over a file an
operator named is not what `init` does.

The alternatives were these:

- **Leave an out-of-vault schema to its operator.** `vault migrate` would refuse a vault whose
  schema lives elsewhere, and the operator would edit the file by hand. That moves the
  comment-preserving rewrite, the one thing a migration owes, onto every operator who shares
  a schema file across machines.
- **Name the written file in the transition.** A plan would carry an absolute path outside
  the vault, which is a wire change and puts machine-local paths into plans that are previewed
  on one host and sent to another.
- **Stage the shadow beside the source.** This is the sibling placement ADR 0011 rules out,
  and a schema directory is often committed or synced.

The ruling has these costs:

- **A plan's transition does not say where its bytes land.** A reader of a schema
  transition learns the role, not the file. The file is the one the registration reads when
  the plan is applied.
- **A re-sent plan lands where the registration reads the schema then.** A `vault set`
  between a preview and its apply moves the target. The before-state hash guards it: a
  source holding other bytes refuses the plan as drift, and the plan lands only over the
  bytes it was composed from.
- **What the caller previewed applies, except where the schema lands.** [ADR
  0032](0032-a-file-state-says-whether-its-bytes-are-a-document.md) takes a resolved plan's
  preconditions from planning time, so what the caller previewed is what applies, and it
  guards the plan with the vault root's identity, which the plan carries. For the schema's
  role, the file the bytes land at is resolved when the plan is applied. An out-of-vault
  anchor's identity is read at staging, because the plan carries none. The guards there are
  the before-state hash and that staging-time identity. A folder replaced between staging and
  publication is answered as a write that failed, naming the folder, never as the vault root
  changing.
- **A write across filesystems is refused**, rather than published by another means.
- **An out-of-vault landing records no own write.** The own-write ledger names vault paths
  only. The watcher's control-file facts are discarded before they reach the lifecycle, so
  nothing reads the missing record. The verb that wrote the file reloads the vault itself.
