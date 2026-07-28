# Decisions

Architecture decision records for `norn`. Each ADR is a present-tense contract: it states
what *is* decided, not what was discussed. ADRs are re-authored at the layer where they
bind — nothing is bulk-copied from an earlier line, and prior decisions are cited as
evidence by reference, never inherited as authority.

Files are `NNNN-slug.md`, numbered sequentially. To add one, take the next number above
the highest here and add its row to the index below.

Statuses: `accepted`, `proposed`, `deprecated`, `superseded by NNNN`.

## Index

| ADR | Title | Status | Covers |
|---|---|---|---|
| [0001](0001-restart-decision-of-record.md) | Restart decision of record | accepted | The blank-repo bottom-up rebuild under default-EXCLUDE: standing laws, layer numbering, release gating, the three test strata, host topology, lockdown gates, the verb charter, mutations, repair and surfaces, and the graduation gate |

## Related

- [`../architecture.md`](../architecture.md) — the invariant spine and crate map the ADRs
  bind against.
- [`../glossary.md`](../glossary.md) — domain terms.
