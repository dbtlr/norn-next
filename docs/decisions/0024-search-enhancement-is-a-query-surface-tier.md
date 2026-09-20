---
status: accepted
date: 2026-09-20
---

# 0024 — the correctness boundary is findings, plans, resolution, and refusal, and search enhancement is a query-surface tier above a model-free floor

No model output ever becomes a finding, a plan, a resolution, or a refusal. That is the
boundary the deterministic law draws, and it is drawn there rather than at the search verb:
a ranked search answer reaches none of the four, so it may be improved by any pinned local
model a vault has opted into. Lexical search over the transactional full-text pillar is the
model-free floor. It is always available and never changes, and a request may always
narrow to it.

Above the floor, enhancement is a ladder of rungs, each a pinned `(model id, version)` from
the in-binary manifest, enabled per vault by configuration, with weights acquired by an
explicit installation-scope act and never by a query. The vault's enabled set is a
request's default; a request subtracts rungs or forces one, and a forced rung the vault
has not enabled refuses with a typed reason. Every report declares the rungs that ran and
each model's identity, and for each rung that holds state it states how far that state
trails the store; a request-time rung holds none and reports no lag. Repeatability is
stated on the answer rather than assumed. The firewall is carried per
rung: a rung that holds state reaches lane 1 only through the store's feed-read partition,
as [ADR 0021](0021-derived-indexes-split-into-two-lanes.md) already requires of an engine;
a rung that runs at request time holds no state and reaches only a report type. A
threshold on a ranked answer is a bound on that answer, never a refusal.

This narrows nothing ADR 0021 decided and reaches a case it did not: 0021 classifies
derived state by its inputs and its invalidation key, and request-time inference is not
derived state. The alternative was to read "no model in the loop" as barring every model
from the ranked tier. That reading would protect nothing, because the ranked tier already
carried a pinned embedding model as derived state that was never a correctness input, and
it would leave the tier where model-backed expansion and reranking buy the most quality
strictly worse than tools that have it. Embeddings, expansion, and reranking are admitted
as one class under the conditions embeddings already met: pinned, opt-in, declared, and
outside the four protected surfaces.

The costs are accepted and stated. A generative runtime is heavier than an embedding
runtime, lives behind the release feature, stays out of every development build, and gets
a crate home in the map when it lands. Each rung above vectors adds inference before or
after retrieval, so a vault turns on the rungs above them knowing the price. Repeatability
across machines weakens one rung at a time, which is why the report carries its ladder.
