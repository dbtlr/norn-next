---
status: accepted
date: 2026-07-29
---

# 0010 — the frontmatter value model is derived from its consumers

`norn-text`'s public frontmatter vocabulary is an owned value model — null, bool, int, float, string, sequence, string-keyed map — not a YAML model and not a serde type. Every consumer of a parsed value can hold exactly this shape and no more: the store projects into SQLite columns and JSON1, the vault schema addresses fields by string key path, and queries reach indexed columns. YAML expressiveness beyond it (non-string keys, tags, anchors) has no downstream consumer, so carrying it would relocate coercion from one place to every consumer; instead the parse boundary strips it once, loudly, as typed diagnostics. The shape is also the intersection of the frontmatter dialects (YAML, TOML, JSON), keeping the dialect — and the serde-based parser behind the seam — an implementation detail the public contract never names. The decision flips only if the vault schema gains the ability to address, or a store pillar the ability to index, something outside the model.
