---
status: accepted
date: 2026-08-12
---

# 0018 — an unread frontmatter block is an undecodable document

The text layer reads a frontmatter block only up to an authored byte bound and refuses a
longer one unparsed. A document whose block was refused for size is quarantined at rung 2,
under a fourth cause class beside the three [ADR 0013](0013-quarantine-is-per-document.md)
names: it yields no facts, a `document/frontmatter-too-large` finding states the bound, the
heal keeps going, and the entry reaches `Ready` serving every other document. The boundary
ADR 0013 draws is unchanged — this is a defect in one document the vault holds, not a
failure of the environment — and the recovery is the one ADR 0013 already has: a block
rewritten inside the bound is an ordinary derivation whose own findings discard clears the
finding.

**The bound exists because reading a block is superlinear in its length.** The YAML scanner
behind the parse seam costs time quadratic in block length on nested flow collections, so
one ordinary-looking document can carry a block worth seconds of CPU on a worker that has a
whole vault to heal. Length is the input that decides how far that goes, so length is what is
bounded. What that buys is a ceiling and not a flat cost, and the residual is recorded rather
than claimed away: measured in release at the bound, an ordinary mapping reads in about three
milliseconds and the worst nesting in about a quarter of a second, so shape still spans two
orders of magnitude under the bound. The parser's own recursion limit does not help — it
refuses such a block, but only after paying the scan that costs the quarter second. A depth
bound would cut that residual and is not taken here: a quarter second, once, on a document
nobody authored is a cost the heal absorbs, and the bound already removes the unbounded case.
The bound is authored rather than measured — it is two orders of magnitude past
the largest block authored documents carry, so no real frontmatter approaches it and a
machine that gets faster does not move it.

**Quarantine rather than a note beside a derived row, because the fields are what a vault is
queried by.** Deriving the body of a document whose whole block went unread would put a row
in the store answering *this document has no tags, no title, no aliases* — a wrong answer
rather than a missing one, and one that no count of block-scoped notes stops a query from
giving. The trade is that a document loses its body facts too, and that is the honest
reading: a block nobody can read is a document norn does not know, and the finding is where
that is stated. The alternative considered and rejected was the forgiving read the text layer
gives every other malformed block — value absent, note recorded, body derived — which is
right where the *shape* of a value is in doubt and wrong where the whole block is unknown.

**The refusal is never a truncation.** A block read to the bound and cut would parse into a
value nobody wrote and store it as though the document said it, which is worse than either
answer above. The bound refuses the block whole or reads it whole.
