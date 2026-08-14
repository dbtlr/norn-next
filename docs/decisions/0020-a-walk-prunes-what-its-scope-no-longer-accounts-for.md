---
status: accepted
date: 2026-08-13
---

# 0020 — a walk prunes the findings its cleanly-enumerated scope no longer accounts for

Supersedes [ADR 0019](0019-quarantine-is-for-nothing-derivable-causes.md), whose
boundary and degradation this decision restates whole. ADR 0019 recorded one gap in its
consequences: a finding whose subject the vault no longer held was reached by nothing —
no row means no death, and walks read only paths that exist — so a stale finding stood
until a schema re-pin emptied the table. **The subject axis now converges: the walk
itself is the act that reaches such a finding.**

**Quarantine is for the causes that leave nothing derivable.** There are **three cause
classes** — path bytes that are not UTF-8, a path spelling the document-path grammar
refuses, and a body that is not UTF-8 — and each of them denies the deriving act
everything: no identity to store a row under, or no text to read facts out of. The
rung-2 heal skips such a document's facts, records a finding naming the path and the
cause class, and keeps going, so the entry reaches `Ready` and serves every other
document. The store holds only representable truth, so a document that stops decoding
loses its store row; the row's death is recorded as a quarantine — a death of the
derived row, not of the file. **The set is closed: a cause that is not one of the three
named above is a refusal or the degradation below.** The store's own projection bound is
not a fourth outcome: it refuses a frontmatter value nesting deeper than the store
projects, and that refusal would withdraw a whole increment rather than one document —
so it stands above what any readable block can carry. The text layer stops nesting
through a block first, and a block it will not read is the degradation below.

**A wholly unread frontmatter block degrades: the row the act could derive, plus a
standing finding.** A block that never closes, a block that is not well-formed YAML, and
a block past the authored `FRONTMATTER_MAX_BYTES` bound are one situation with three
causes — the block is unread, and the document's fields are unknown. All three answer
alike: the document keeps its identity and its body facts, its frontmatter value is
absent, and a **document-scoped finding naming the cause** stands at the document's own
path beside its row. The document stays a document, and the unread block becomes a
recorded, queryable defect a validator can surface so that somebody fixes it. A document
vanishing over a YAML typo misleads more queries than a partial row carrying a reachable
signal does — that is the trade, and it is why the exclusivity gives way rather than the
row.

**The findings model carries two scopes, and they differ in exactly one thing: whether a
document row at the subject withholds the finding.** A **place-scoped** finding — every
quarantine — is row-exclusive: a place a real document occupies is that document's, so a
quarantine standing there is withheld rather than filed over a readable document.
A **document-scoped** finding — every unread block — is co-resident: its subject is the
document it is about, so the row standing there is the thing it describes. Both scopes
are discarded by the subject axis; the increment's whole-subject discard stays whole,
licensed per scope — a place-scoped finding at a row-bearing place is withheld for as
long as the row stands, and a document-scoped finding is taken and refiled by the same
act, because the act that wrote the row is the act that read the frontmatter.

**The row and the finding beside it are what a heal converges, not the row alone.** A
partial row is worth writing because a reachable signal stands beside it, so a heal
re-derives a degraded document whenever no document-scoped finding stands at it, rather
than only when its bytes moved. The row states its own defect — an absent frontmatter
projection beside a nonzero count of frontmatter-scoped diagnostics is a block nothing
read.

**A walk prunes the findings its cleanly-enumerated scope no longer accounts for.** One
rule for every walk, with the vault heal as the maximal case rather than a special
venue: at job end, in the same recording that files the walk's own findings, the walk
discards each finding whose subject lies inside a scope it enumerated cleanly and that
nothing it read accounted for. The deletion of a quarantined document converges at the
deletion's own scoped heal, never waiting for an attach or an explicit full heal.
Authority is earned by clean enumeration, and three withholders bound it: a walk that
refuses concludes nothing; a root the walk did not enter covers the places beneath it —
and, where the root's own spelling defeats the address grammar, every marker-carrying
place, because the places it hides carry the marker rather than its segments; a single
name the walk opened nothing at withholds the one rendered place its spelling renders
onto. A rendered place is accounted for while any entry the walk read renders onto it,
and a scope rooted below a place's deepest unrendered ancestor concludes nothing there.
The prune writes through the same job-end findings path as every producer, so whatever
durability that recording carries, the prune carries identically — a process killed
before it leaves exactly the state a heal that never pruned leaves.

**The mechanism is a join against the rows the walk already converges, never a
per-place stamp.** A row standing at a subject is itself the walk's account of that
place — it withholds a place-scoped finding and is the thing a document-scoped one
describes — so the store answers the accounting by key: one paged read names the
subjects in a scope that hold a walk-derived finding with no row standing at them, and
the job carries only what the store cannot know. A generation stamp would cost a write
per document read per heal — a write per document on a converged vault — which the
warm-zero derivation bar forbids.

**The bound itself is untouched.** A block past `FRONTMATTER_MAX_BYTES` is still refused
unparsed and never truncated: reading a block is superlinear in its length, the bound is
the ceiling on what one block costs, and a block read to the bound and cut would parse
into a value nobody wrote. What the degradation changes is the posture the refusal
produces, not the refusal.

## Consequences

**The churn bars lose a row-flip family.** A document edited across the size bound never
flips its row: the row stands and only its finding appears and clears. The remaining row
flips are the three quarantine classes, each a document that genuinely stops being
derivable.

**The posture branches on typed document state.** A wholly unread block is a state the
text layer reports — the refusal reason, beside the note it also files — and a
diagnostic code still crosses no seam.

**A degraded row and its finding converge as a pair, at the cost of one indexed findings
lookup per defective document per heal.** A converged vault re-derives nothing.

**A finding whose subject the vault no longer holds is reached by the next walk whose
clean scope covers its place.** The gap ADR 0019 recorded is closed on the subject axis:
the stale finding's end is the ordinary convergence of the walk, and the schema re-pin
is again just a discard among discards rather than the only end a stale finding has. The
price is the prune-authority accounting above — a walk that cannot vouch for a place
must visibly withhold it, and every withholder is pinned.

**Inversion.** If partial rows prove untrustworthy in practice — queries answered from a
row whose fields were never read, by callers that never look at the findings beside
it — the revisit is toward quarantine: move the unread-block causes back into the cause
class, and accept that a typo removes a document from the index. If the prune proves
untrustworthy — findings deleted that a withheld or unreadable scope still owed — the
revisit is toward narrower authority, never toward a stamp: shrink what a walk may
vouch for before spending a write per converged document.
