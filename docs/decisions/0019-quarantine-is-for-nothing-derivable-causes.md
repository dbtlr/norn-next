---
status: accepted
date: 2026-08-13
---

# 0019 — quarantine is for nothing-derivable causes, and a wholly unread block degrades to a row plus a finding

Supersedes [ADR 0018](0018-quarantine-cause-classes-are-closed-and-carry-the-unread-block.md),
whose boundary this decision restates whole. ADR 0018 left one asymmetry open — a malformed
or unclosed in-bounds frontmatter block is wholly unread and derives a row, while a block
past the authored bound is wholly unread and quarantines — and resolved it in favour of the
size class by leaning on an exclusivity the findings model no longer keeps. **The exclusivity
relaxes, and both cases degrade alike.**

**Quarantine is for the causes that leave nothing derivable.** There are **three cause
classes** — path bytes that are not UTF-8, a path spelling the document-path grammar
refuses, and a body that is not UTF-8 — and each of them denies the deriving act everything:
no identity to store a row under, or no text to read facts out of. The rung-2 heal skips
such a document's facts, records a finding naming the path and the cause class, and keeps
going, so the entry reaches `Ready` and serves every other document. The store holds only
representable truth, so a document that stops decoding loses its store row; the row's death
is recorded as a quarantine — a death of the derived row, not of the file. **The set is
closed: a cause that is not one of the three named above is a refusal or the degradation
below.**

**A wholly unread frontmatter block degrades: the row the act could derive, plus a standing
finding.** A block that never closes, a block that is not well-formed YAML, and a block past the authored
`FRONTMATTER_MAX_BYTES` bound are one situation with three causes — the block is unread, and the document's fields are
unknown. All three answer alike: the document keeps its identity and its body facts, its
frontmatter value is absent, and a **document-scoped finding naming the cause** stands at
the document's own path beside its row. The document stays a document, and the unread block
becomes a recorded, queryable defect a validator can surface so that somebody fixes it.

**A document vanishing over a YAML typo misleads more queries than a partial row carrying a
reachable signal does.** That is the trade, and it is why the exclusivity gives way rather
than the row. Quarantine answers "norn does not know this document" by removing it, which is
right when norn really knows nothing about it and wrong when norn knows its body, its
headings, its links and its tags. The wrong answer ADR 0018 avoided — *this document has no
tags, no title, no aliases*, stated with no reachable signal that the fields were never
read — is avoided here by the signal rather than by the removal: the finding is where "the
fields are unknown" is stated, and it is enumerable at the document's own path. The common
failure is a typo in a hand-written block, so the common failure gets graceful degradation.

**The findings model carries two scopes, and they differ in exactly one thing: whether a
document row at the subject withholds the finding.**

- A **place-scoped** finding — every quarantine — is row-exclusive. A place a real document
  occupies is that document's, so a quarantine standing there is withheld rather than filed
  over a readable document, and the heal that removes the colliding document is the one that
  files it.
- A **document-scoped** finding — every unread block — is co-resident. Its subject is the
  document it is about, so the row standing there is the thing it describes and withholding
  it would suppress every finding it exists to report.

Both scopes are discarded by the subject axis, and the increment's own whole-subject discard
stays whole: **a change that writes a row at a place ends every finding standing there**, and
what licenses it is now per scope rather than one exclusivity. A place-scoped finding at that
place is withheld for as long as the row stands, so taking it costs nothing that could be
read. A document-scoped finding is taken and **refiled by the same act**, because the act
that wrote the row is the act that read the frontmatter: it concludes the block's readability
and records what it concluded, in that order, inside one flush. Closure needs no second
mechanism either way — a block that reads again is an ordinary derivation whose discard takes
the finding with it, and nothing refiles it.

**The bound itself is untouched.** A block past `FRONTMATTER_MAX_BYTES` is still refused
unparsed and never truncated, for the cost reason ADR 0018 states and re-states here: reading
a block is superlinear in its length, the bound is the ceiling on what one block costs, and a
block read to the bound and cut would parse into a value nobody wrote. What changes is the
posture the refusal produces, not the refusal.

## Consequences

**The churn bars lose a row-flip family.** A document edited across the size bound no longer
flips its row: the row stands and only its finding appears and clears. The remaining row
flips are the three quarantine classes, each of which is a document that genuinely stops
being derivable.

**The posture branches on typed document state.** A wholly unread block is a state the text
layer reports — the refusal reason, beside the note it also files — rather than something a
consumer recovers by scanning a note list for a code. A diagnostic code still crosses no
seam.

**A schema re-pin discards a co-resident finding that the walk after it does not refile.**
The re-pin invalidates every finding, and the vault-wide walk that follows is
hash-authoritative: it reads every path no document row stands at, and re-derives a path that
has one only when its content hash moved. A place-scoped finding stands where no row does and
is therefore refiled; a document-scoped finding stands where a row does, so it returns when
the document next changes rather than with the walk. This is a convergence gap in the same
family as a finding whose subject left the vault, and it is stated as one rather than
answered here — the answer is a subject-axis pass over the findings table, which is where a
future validator's findings, which also stand beside rows, will need it too.

**Inversion.** If partial rows prove untrustworthy in practice — queries answered from a row
whose fields were never read, by callers that never look at the findings beside it — the
revisit is toward quarantine: move the unread-block causes back into the cause class, and
accept that a typo removes a document from the index.
