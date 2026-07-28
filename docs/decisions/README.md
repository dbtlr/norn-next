# Decisions

Architecture decision records for `norn`. Each ADR is a present-tense contract: it states
what *is* decided about the system as it stands, not how the project arrived there.

**Decisions land layered.** An ADR is written at the layer where its decision binds, as that
layer lands — not in advance, and never as one document covering everything. Files are
`NNNN-slug.md`, numbered sequentially from `0001`. To add one, take the next number above
the highest here and add its row to the index below.

**Division of labour.** Cross-layer invariants and crate boundaries live in
[`../architecture.md`](../architecture.md), which governs them; decisions that bind at a
single layer land here as ADRs. When a new ADR changes something the spine states, the spine
is updated in the same change — it does not become stale with a footnote.

Statuses: `accepted`, `proposed`, `deprecated`, `superseded by NNNN`.

Mimir artifact **NORN-a1** is the frozen pre-repository *design* record — the thinking that
preceded this tree. It is consultable as evidence and is cited as authority by nothing;
`../architecture.md` is what governs today. This holds however many ADRs are listed below.

## Index

*No ADRs yet.*

## Related

- [`../architecture.md`](../architecture.md) — the invariant spine and crate map, and the
  contract reviews enforce against today.
- [`../glossary.md`](../glossary.md) — domain terms.
