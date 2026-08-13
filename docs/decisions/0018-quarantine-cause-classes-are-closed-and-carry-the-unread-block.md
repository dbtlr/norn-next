---
status: superseded
superseded-by: 0019-quarantine-is-for-nothing-derivable-causes.md
date: 2026-08-12
---

# 0018 — quarantine is per document under a closed set of four cause classes

Supersedes [ADR 0013](0013-quarantine-is-per-document.md), whose boundary this decision
restates whole with one more cause class and the rule that actually forces the posture.

A document norn cannot decode is quarantined rather than refused: the rung-2 heal skips its
facts, records a finding naming the path and the cause class (withheld while a readable
document stands at the finding's rendered spelling), and keeps going, so the entry reaches
`Ready` and serves every other document. There are **four cause classes** — path bytes that
are not UTF-8, a path spelling the document-path grammar refuses, a body that is not UTF-8,
and a frontmatter block past the authored byte bound the text layer reads — and every rung-2
path answers them the same way, because a document the vault holds and norn cannot read is a
fact about that document rather than about the vault. The store holds only representable
truth, so a document that stops decoding loses its store row; the row's death is recorded as
a quarantine — a death of the derived row, not of the file. Recovery needs no separate
mechanism: a document that reads again is an ordinary derivation, and the increment's own
findings discard takes the finding with it. **The set is closed: a cause that is not one of
the four named above is a refusal.**

**Environmental failures are never quarantined, and that prohibition is what makes the rest
safe.** A schema that will not read, a store that will not open, a walk that cannot list a
directory, a path whose permissions were revoked — each of these refuses and leaves the
entry untrusted, because the environment is broken and the stored state is not. Quarantine
turns "this cannot be read" into "this is absent, and here is why", which is correct exactly
when the vault really does hold something unreadable and catastrophic when the vault is fine
and the environment is not. The boundary is the class of causes, not the shape of the
failure.

**The fourth class exists because reading a block is superlinear in its length.** The YAML
scanner behind the parse seam costs time quadratic in block length on nested flow
collections, so one ordinary-looking document can carry a block worth seconds of CPU on a
worker that has a whole vault to heal. Length is the input that decides how far that goes,
so length is what is bounded. What that buys is a ceiling and not a flat cost, and the
residual is recorded rather than claimed away: measured in release at the bound, an ordinary
mapping reads in about three milliseconds and the worst nesting in about a quarter of a
second, so shape still spans two orders of magnitude under the bound. The parser's own
recursion limit does not help — it refuses such a block, but only after paying the scan that
costs the quarter second. A depth bound would cut that residual and is not taken here: a
quarter second, once, on a document nobody authored is a cost the heal absorbs, and the
bound already removes the unbounded case. The bound is authored rather than measured — two
orders of magnitude past the largest block authored documents carry, so no real frontmatter
approaches it and a machine that gets faster does not move it.

**Quarantine rather than a derived row, because findings and rows are mutually exclusive by
construction.** The findings machinery drops any finding standing at a path that holds a
document row — a place a real document occupies is that document's, and a finding there
would call a document that just derived unreadable. So there is no "note beside a derived
row" posture available: deriving the body of a size-refused document would put a row in the
store answering *this document has no tags, no title, no aliases* with no reachable signal
that the fields were never read. Quarantine is the one posture that surfaces the fact. The
trade is that the document loses its body facts too, and that is accepted: the finding is
where "norn does not know this document" is stated.

**One asymmetry is open, deliberately.** A malformed or unclosed in-bounds block is also
wholly unread, yet such a document derives a row today — value absent, a block-scoped note
counted on the row, body facts kept. Only length separates the two treatments. Whether that
case joins this cause class, or the findings model instead relaxes so a document-scoped
note can stand beside a row and both cases degrade alike, is not ruled here: the first
removes documents from the index over a typo, the second reshapes the findings exclusivity
this decision leans on. The asymmetry is recorded as an open ruling rather than resolved as
a side effect of a size bound.

**The refusal is never a truncation.** A block read to the bound and cut would parse into a
value nobody wrote and store it as though the document said it, which is worse than either
answer above. The bound refuses the block whole or reads it whole.
