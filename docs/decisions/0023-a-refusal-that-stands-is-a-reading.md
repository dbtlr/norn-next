---
status: accepted
date: 2026-09-14
---

# 0023 — a refusal that stands is a reading

Supersedes [ADR 0020](0020-a-walk-prunes-what-its-scope-no-longer-accounts-for.md), whose
boundary this decision restates whole. ADR 0020 earned prune authority by clean enumeration
and bounded it with three withholders, one of which was **a root the walk did not enter**.
That clause read a deliberate refusal and a name the walk could not look at as one situation.
They are two. **A refusal that stands is itself a reading, and the walk prunes what it
accounts for.**

**Quarantine is for the causes that leave nothing derivable.** There are **three cause
classes** — path bytes that are not UTF-8, a path spelling the document-path grammar
refuses, and a body that is not UTF-8 — and each of them denies the deriving act
everything: no identity to store a row under, or no text to read facts out of. The rung-2
heal skips such a document's facts, records a finding naming the path and the cause class,
and keeps going, so the entry reaches `Ready` and serves every other document. The store
holds only representable truth, so a document that stops decoding loses its store row; the
row's death is recorded as a quarantine — a death of the derived row, not of the file. **The
set is closed: a cause that is not one of the three named above is a refusal or the
degradation below.** The store's own projection bound is not a fourth outcome: it refuses a
frontmatter value nesting deeper than the store projects, and that refusal would withdraw a
whole increment rather than one document — so it stands above what any readable block can
carry. The text layer stops nesting through a block first, and a block it will not read is
the degradation below.

**A wholly unread frontmatter block degrades: the row the act could derive, plus a standing
finding.** A block that never closes, a block that is not well-formed YAML, and a block past
the authored `FRONTMATTER_MAX_BYTES` bound are one situation with three causes — the block
is unread, and the document's fields are unknown. All three answer alike: the document keeps
its identity and its body facts, its frontmatter value is absent, and a **document-scoped
finding naming the cause** stands at the document's own path beside its row.

**The findings model carries two scopes, and they differ in exactly one thing: whether a
document row at the subject withholds the finding.** A **place-scoped** finding — every
quarantine — is row-exclusive: a place a real document occupies is that document's, so a
quarantine standing there is withheld rather than filed over a readable document. A
**document-scoped** finding — every unread block — is co-resident: its subject is the
document it is about, so the row standing there is the thing it describes. Both scopes are
discarded by the subject axis; the increment's whole-subject discard stays whole, licensed
per scope.

**The row and the finding beside it are what a heal converges, not the row alone.** A heal
re-derives a degraded document whenever no document-scoped finding stands at it, rather than
only when its bytes moved.

**A walk prunes the findings its cleanly-enumerated scope no longer accounts for.** One rule
for every walk, with the vault heal as the maximal case rather than a special venue: at job
end the walk discards each finding whose subject lies inside a scope it enumerated cleanly
and that nothing it read accounted for. The deletion of a quarantined document converges at
the deletion's own scoped heal, never waiting for an attach or an explicit full heal.

**Authority is earned by reading, and a refusal that stands is a reading.** An exclusion
root, norn's own mechanism subtree, a shadow basename, a symbolic link, a device-like entry,
a name below an entry the walk reads rather than descends into — each is a fact about an
entry that is there, and a derivation begun from zero over the same tree refuses it
identically. So the places beneath such a root hold what that derivation holds: no row and
no finding. The walk prunes them. Where the root's own spelling defeats the address grammar
the places it hides are reachable only as the marker-carrying places they render onto, and
the prune reaches them that way — by the marker the places carry, not by a prefix the store
cannot name.

**Two withholders remain, and both are the walk saying it could not look.** A walk that
refuses concludes nothing: its error ends the job ahead of its own prune, so every finding
in its scope stands. And a root that **vanished** inside one of the walk's own windows, or a
single name it enumerated and opened nothing at, covers the places beneath it: nothing was
read there and what stands there now is a question the walk never asked. Where such a
vanished root's own spelling defeats the address grammar the hold reaches every
marker-carrying place the job would otherwise conclude, because which of them the root hid
is unknowable from outside it. Beside those two stands the rule that is not a withholder but
a bound on scope: a place is named by every spelling that renders onto it, so a walk rooted
below that place's deepest unrendered ancestor read some of those spellings and never the
rest and concludes nothing there.

**The mechanism is a join against the rows the walk already converges, never a per-place
stamp.** A row standing at a subject is itself the walk's account of that place, so the
store answers the accounting by key: one paged read names the subjects in a scope that hold
a walk-derived finding with no row standing at them. A generation stamp would cost a write
per document read per heal, which the warm-zero derivation bar forbids.

**The bound itself is untouched.** A block past `FRONTMATTER_MAX_BYTES` is still refused
unparsed and never truncated.

## Rationale

**Certification property 1 is the bar: the maintained store equals a build from zero under
the same exclusions.** ADR 0020's third withholder broke it. A directory that becomes a
symbolic link is a root every walk from now on refuses, so a build from zero holds no
finding beneath it — while the maintained store kept every finding that stood there, with
nothing able to reach them again, because the rule read the notation rather than the reason.
Where the refused root's own spelling defeated the address grammar the hold was job-wide
over every marker-carrying place, so one link nobody named froze every undecodable place in
the vault, permanently, in every job.

The direction is the one ADR 0020's inversion clause forbade — this widens prune authority
rather than narrowing it — and that clause is what makes this a supersession rather than an
amendment. What licenses the widening is that the authority was never absent: the walk reads
the refusal, and the refusal is a total statement about everything beneath the entry for as
long as the entry stands. ADR 0020 paid for its gap with visible withholding and pinned
every withholder. This keeps that discipline and pins the narrowed set; what it withdraws is
a hold that was standing in for a reading the walk had already made.

## Consequences

**A scoped leg still concludes less than the vault heal, and they agree on authority rather
than on outcome.** Where the document grammar refuses a refused root's own spelling, the
root stands at a rendered place that every sibling whose rendering lands there also names. A
leg answering one dirty path read one of those spellings, so it converges the range below
the root and leaves that place alone; the vault heal enumerates them all and takes it. The
store converges on the walk that earned the place, which is the pre-existing bound on scope
rather than a new divergence.

**The two axes travel together but not in one transaction.** Rows die inside the prune's own
increment and the findings are taken once every scope of the job has run. A process killed
between the two leaves rows pruned and findings standing — the state a heal that never
pruned leaves — and the next walk that enumerates those places takes them by this same rule.

**A new skip reason must decide whether it stands.** The reason set is matched exhaustively,
so a reason added later cannot inherit an answer; the seam that converges a dirty path asks
for the answer rather than assuming it.

**Inversion.** If the prune proves untrustworthy — findings deleted that a walk earned no
authority over — the revisit is toward narrower authority, never toward a stamp: shrink what
a refusal is read to state before spending a write per converged document. A refusal class
that turns out not to bind a derivation from zero the same way is withdrawn from `stands`
one variant at a time, not by restoring the notation-level hold.
