---
status: accepted
date: 2026-09-24
---

# 0026 — a derived store records the derivation that wrote it, and a digest of derived rows holds the record honest

A derived store rebuilds from zero when its DDL fingerprint differs from the build's, when the
path order its rows were derived under differs from the one its root proves, or when it is
damaged; between rebuilds, a file is derived again only when its content hash moves, and
rows keyed by the schema are derived again when the pinned schema moves. None of those
triggers sees **a change to what derivation writes for unchanged input**: a parser that
trims a heading differently, a fact extractor that records one more field, a resolver that
files a class under another key. A store derived by the previous build keeps its rows
until each file is edited, so it answers differently from a store derived from zero, and
the same vault answers a read differently depending on when its store was derived.

**The deriver names a derivation version, and the store records it beside its DDL
fingerprint as a rebuild input.** An open that finds another version, or none, rebuilds
from zero through the same ceremony a fingerprint mismatch uses, and reports why. The
version is the deriver's, because derivation is what it describes; the store records and
judges it and knows nothing of what changed. The version is norn-host's and covers every
crate's contribution to the written rows — norn-text's parse, norn-host's plan, and
norn-store's write-time computation such as suffix keys, projection hashes, sub-fingerprints
and the full-text index — and the digest is taken through a real attach, so the store's
write-time computation is inside it. A recorded value no build writes, or one that is not
text, rebuilds like a moved version, as a recorded path order does.

**A digest of derived rows holds the version honest.** A test derives a pinned corpus from
zero and digests every derived row; the digest is pinned beside the version it was taken
under. A change to derivation output moves the digest, and the test fails until the
version moves with it, so no change to what derivation writes can land without forcing the
rebuild it needs. This is the pattern the regression registry's contract digest uses: a
reviewed number that a change must move on purpose.

Three alternatives were weighed. Moving the DDL fingerprint by hand whenever derivation
changes forces the rebuild once, but relies on someone noticing, and nothing catches the
change that was not noticed. Stamping each derived row with the derivation that wrote it
supports re-deriving only the stale rows, at the cost of per-row bookkeeping and a partial
re-derivation path for a rare event, when a rebuild from zero is already the store's
answer to a changed input. Hashing the derivation code rebuilds on every refactor that
changes no output.

The price is one rebuild of every derived store per derivation change, which the pre-1.0
posture accepts and which, after 1.0, is the vault's derivation time. The digest's corpus
has to exercise the facts derivation writes, or a change to an unexercised fact slips
past it; the corpus grows with each fact a derivation change touches. A sidecar engine's
store keeps its own inputs (its model identity); this record governs the lane-1 store.
