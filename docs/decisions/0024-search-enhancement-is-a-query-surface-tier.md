---
status: accepted
date: 2026-09-20
---

# 0024 — the correctness boundary is findings and plans, and search enhancement is a query-surface tier above a model-free floor

Ranked search answers feed no finding and no plan. That is the boundary the deterministic
law draws, and it is drawn there rather than at the search verb: no model output ever
becomes a finding, a plan, or a repair input, while a ranked answer may be improved by
any pinned local model a vault has opted into. Lexical search over the transactional
full-text pillar is the model-free floor. It is always available, it never changes, and
`--mode lexical` is the exact-repeatability form on any vault.

Above the floor, enhancement is a ladder of rungs — vector nearest, query expansion,
reranking — each a pinned `(model id, version)` from the in-binary manifest, enabled per
vault by configuration, with weights acquired by an explicit machine-local act and never
by a query. The vault's enabled set is a request's default; a request subtracts rungs or
forces one, and a forced rung the vault has not enabled refuses with a typed reason. Every
report declares the rungs that ran, each model's identity, and the semantic cursor lag
against the store epoch, so repeatability is stated on the answer rather than assumed.
The blind-crate invariant covers every rung: nothing in a model runtime can reach findings
or plans.

The alternative was to read "no LLM in the loop" as barring every model from the ranked
tier, which qmd's hybrid pipeline shows is the tier where model-backed expansion and
reranking buy the most. Reading the law that way would leave norn strictly worse than a
tool it means to subsume while protecting nothing, because the ranked tier already
carried a pinned embedding model as derived state that was never a correctness input.
Embeddings, expansion, and reranking are one class under that reading, and the class is
admitted under the same conditions embeddings already met: pinned, opt-in, declared, and
firewalled from findings and plans. A single verb with a mode switch was preferred over
qmd's three verbs because ranking, not the model count, is the one line that separates a
ranked answer from a structured one.

The costs are accepted and stated. A generative runtime is heavier than an embedding
runtime and lives behind the same release feature, out of every development build. Each
rung above vectors adds inference before or after retrieval, so "cheap and fast" is a
property of the floor and of vectors, and a vault turns on the rungs above them knowing
the price. Repeatability across machines weakens one rung at a time, which is why the
report carries its ladder.
