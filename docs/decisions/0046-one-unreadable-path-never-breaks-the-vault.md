---
status: proposed
date: 2026-10-26
---

# 0046 — one unreadable path never breaks the vault: it is marked broken and skipped

Supersedes [ADR 0023](0023-a-walk-refusal-that-stands-is-a-reading.md), whose boundary this
decision restates whole. ADR 0023 inherited a rule that reaches back through ADR 0020 to
[ADR 0013](0013-quarantine-is-per-document.md): a walk that cannot read concludes nothing and
ends its job, so a directory the walk cannot list refuses the heal, and a document the heal
cannot open is refused rather than quarantined, because widening quarantine past the
undecodable-document class would turn a broken environment into silent data loss. That rule
let one file's permissions refuse the whole vault. **The vault never breaks because one file
did.** A document a heal cannot open is marked broken and skipped, a directory a walk cannot
list is withheld and marked the same way, and the heal goes on. The data-loss hazard the
earlier rule guarded against is avoided by never pruning what was not read, not by refusing
the vault.

**The word is used in four senses here, and each keeps a distinguishing word.** A
**refusal** in the glossary's sense is a resolved outcome in which norn performs no requested
mutation. A **walk refusal** is a root a walk deliberately passes over and reads nothing
under. A **broken path** is one document the walk named and could not open, or one directory
it reached and could not list, because opening, listing or reading that path failed — permission
denied, the operation not permitted on it, or an I/O error on that path. A **failure of the
environment as a whole** is the fourth: a vault schema that will not read, a store that will not
open, a vault root that is gone or that coverage cannot be installed over, and the process's own
resource failures — an exhausted descriptor table or memory — which say nothing about the path
being read. Those end the job and keep their existing rules; this decision touches them only as a
withholder. A device failing under many paths marks each path it fails as broken; the vault does
not break because its files did.

**Quarantine is for the causes that leave nothing derivable.** There are **three cause
classes** — path bytes that are not UTF-8, a path spelling the document-path grammar refuses,
and a body that is not UTF-8 — and each of them denies the deriving act everything: no
identity to store a row under, or no text to read facts out of. The rung-2 heal skips such a
document's facts, records a finding naming the path and the cause class, and keeps going, so
the entry reaches `Ready` and serves every other document. The store holds only representable
truth, so a document that stops decoding loses its store row; the row's death is recorded as
a quarantine — a death of the derived row, not of the file. **The set is closed: a cause that
is not one of the three named above is a walk refusal, a broken path, the degradation below,
or a failure of the environment as a whole.** The store's own projection bound is not a
fourth outcome: it refuses a frontmatter value nesting deeper than the store projects, and
that refusal would withdraw a whole increment rather than one document — so it stands above
what any readable block can carry. The text layer stops nesting through a block first, and a
block it will not read is the degradation below.

**A wholly unread frontmatter block degrades: the row the act could derive, plus a standing
finding.** A block that never closes, a block that is not well-formed YAML, and a block past
the authored `FRONTMATTER_MAX_BYTES` bound are one situation with three causes — the block is
unread, and the document's fields are unknown. All three answer alike: the document keeps its
identity and its body facts, its frontmatter value is absent, and a **document-scoped finding
naming the cause** stands at the document's own path beside its row. The document stays a
document, and the unread block becomes a recorded, queryable defect a validator can surface
so that somebody fixes it. A document vanishing over a YAML typo misleads more queries than a
partial row carrying a reachable signal does — that is the trade, and it is why the
exclusivity gives way rather than the row.

**A document a heal cannot open is broken and skipped.** Every rung-2 path answers it the
same way — a from-zero build, the full tree heal, a scoped subtree heal and the warm scoped
increment:

- **Its row stays.** An open the machine denies is not evidence that the document left, so
  its name keeps resolving and links to it still resolve to it. A row a store already
  committed for it is not pruned and keeps the facts it holds, save one: its content hash is
  cleared, because no bytes now stand behind it. A document that holds no row — the first
  build meeting it, or a file created unreadable — gets the row its path alone derives, its
  names and nothing else, which is the row a from-zero build's first step writes for it.
- **Nothing is derived from bytes nobody read.** The heal writes no content, frontmatter or
  structure fact for it, and replaces none a committed row already holds.
- **A document-scoped finding names the cause** and stands at the document's own path beside
  its row, as an unread block's finding does: an **unreadable-document** finding, carrying the
  cause the machine gave. Its kind is spelled in `norn-wire`, which owns the finding-kind
  vocabulary. The finding and the cleared hash commit in one transaction.
- **The heal continues**, and the entry reaches `Ready` serving every other document and this
  document's row.

**A directory a walk cannot list is withheld and marked, and the walk continues.** Its places
are not pruned: every row and every finding beneath it stands, as ADR 0023 already withheld
the findings beneath a page it could not read, because nothing was read there and what stands
there now is a question the walk could not ask. A place-scoped **unreadable-directory**
finding stands at the directory's own path, carrying the cause, and the walk goes on through
every other directory and concludes everywhere else.

**A name the walk listed but whose stat the machine denies is the same condition at one
name**: what stands at it and beneath it stays, and a finding at the name says it could not
be read. Which finding depends on what stands there, since whether the name is a document or
a directory is exactly what the stat could not say. Where a document row stands at the name,
the name takes the document posture: an unreadable-document finding beside the row, whose
content hash is cleared as a broken document's is. A `.md` name holding no row takes the same
posture and gets the names-only row, since its path alone derives the names, as it does for
any document that cannot be opened. Every other name takes the directory posture: a
place-scoped unreadable-directory finding at the name, with what stands beneath it withheld.

**The watcher files no findings.** On a backend that installs a watch per directory, the
install can meet a directory's denial before the walk does. That is this directory's
condition, not a lost watcher, so the install withholds coverage at that directory alone
rather than withdrawing the entry, and it marks nothing: the walk that meets the same denial
files the finding. The directory's parent is still watched, and its watch reports a change
to the directory's attributes. That report requeues the directory, as any filesystem fact
requeues its path, and the walk that lists it installs coverage over it.

**Recovery needs no second mechanism.** A broken document that opens again is an ordinary
derivation: its row holds no content hash, so the bytes the walk reads match nothing, even
bytes identical to those it read before the path broke, and the act writes its row, ends
every finding standing at it and files what it concludes. A directory that lists again is an
ordinary enumeration: the walk reads what is beneath it, and the subject-axis prune below
takes the unreadable-directory finding, since nothing the walk read accounts for it. A broken
document that leaves the vault leaves as any document does.

**A broken path is a state of its own, not a fourth quarantine cause class.** Quarantine is a
death of the derived row, recorded by a place-scoped finding where no row stands: it states
that the bytes are there and nothing can be derived from them. A broken path states neither.
Its bytes were not read, so nothing is known about them, and killing its row is exactly the
data loss the earlier rule feared. Admitting it to the closed set would either kill rows the
store had no evidence against or stretch the class until a quarantine no longer means the row
died. The broken path keeps the closed set closed and meaning one thing, and it takes the
posture whose shape it shares: a row plus a co-resident finding, like an unread block —
except that its row holds only what was read before, never what the act derived now.

**The findings model carries two scopes, and they differ in exactly one thing: whether a
document row at the subject withholds the finding.** A **place-scoped** finding — every
quarantine, and every unreadable-directory finding — is row-exclusive: a place a real
document occupies is that document's, so a finding standing there is withheld rather than
filed over a readable document. A **document-scoped** finding — every unread block, and every
unreadable-document finding — is co-resident: its subject is the document it is about, so
the row standing there is the thing it describes. Both scopes are discarded by the subject
axis; the increment's whole-subject discard stays whole, licensed per scope — a place-scoped
finding at a row-bearing place is withheld for as long as the row stands, and a
document-scoped finding is taken and refiled by the same act, because the act that wrote the
row is the act that read the frontmatter, or tried to open the document.

**The row and the finding beside it are what a heal converges, not the row alone.** A
partial row is worth writing because a reachable signal stands beside it, so a heal
re-derives a degraded document whenever no document-scoped finding stands at it, rather than
only when its bytes moved. The row states its own defect — an absent frontmatter projection
beside a nonzero count of frontmatter-scoped diagnostics is a block nothing read. A broken
document's row states its defect too: **a row holding no content hash matches no bytes**, so
every walk that reads the document again writes its row, as it would for a changed document.
A walk already reads every document it enumerates, because only a content hash concludes
"unchanged", so the signal costs no lookup in the findings beside the row: like every other
piece of pending maintenance, it rests in the row itself.

**A walk prunes the findings its cleanly-enumerated scope no longer accounts for.** One rule
for every walk, with the vault heal as the maximal case rather than a special venue: at job
end the walk discards each finding whose subject lies inside a scope it enumerated cleanly and
that nothing it read accounted for. The deletion of a quarantined document converges at the
deletion's own scoped heal, never waiting for an attach or an explicit full heal. **A
rendered place is accounted for while any entry the walk read renders onto it**, and a scope
rooted below a place's deepest unrendered ancestor concludes nothing there. A broken
document's row accounts for its place as every row does.

**Authority is earned by reading, and a walk refusal that stands is a reading.** An exclusion
root, norn's own mechanism subtree, a shadow basename, a symbolic link, a device-like entry, a
name below an entry the walk reads rather than descends into — each is a fact about an entry
that is there, and a derivation begun from zero over the same tree refuses it identically. So
the places beneath such a root hold what that derivation holds: no row and no finding. The
walk prunes them.

**What a leg may prune is still bounded by the spellings it enumerated.** Where the refused
root's own spelling defeats the address grammar, the places it hides are named only as the
marker-carrying places they render onto, and a scope that enumerated every spelling rendering
onto those places — the vault heal — reaches them that way, by the marker the places carry
rather than by a prefix the store cannot name. A leg answering one dirty path enumerated one
such spelling and never its siblings, so it leaves those places to the next heal; where the
root's spelling admits no prefix at all there is no row beneath it for that leg to converge
either, since the spelling poisons every path under it. The two legs agree on authority, not
on outcome.

**Three withholders remain, and each is the walk saying it could not look.** A failure of the
environment as a whole concludes nothing: its error ends the job ahead of its own prune, so
every finding in its scope stands. A directory the walk cannot list, or a name whose stat the
machine denies, withholds every row and every finding at and beneath it, and the job concludes
everywhere else. And a root that **vanished** inside one of the walk's own windows, or a
single name it enumerated and opened nothing at because nothing was there, covers the places
beneath it: nothing was read there and what stands there now is a question the walk never
asked. Where such a vanished or unlistable root's own spelling defeats the address grammar the
hold reaches every marker-carrying place the job would otherwise conclude, because which of
them the root hid is unknowable from outside it.

**The mechanism is a join against the rows the walk already converges, never a per-place
stamp.** A row standing at a subject is itself the walk's account of that place — it
withholds a place-scoped finding and is the thing a document-scoped one describes — so the
store answers the accounting by key: one paged read names the subjects in a scope that hold a
walk-derived finding with no row standing at them, and the job carries only what the store
cannot know, the directories it could not list among them. A generation stamp would cost a
write per document read per heal — a write per document on a converged vault — which the
warm-zero derivation bar forbids.

**The bound itself is untouched.** A block past `FRONTMATTER_MAX_BYTES` is still refused
unparsed and never truncated: reading a block is superlinear in its length, the bound is the
ceiling on what one block costs, and a block read to the bound and cut would parse into a
value nobody wrote. What the degradation changes is the posture the refusal produces, not the
refusal.

## Rationale

**Refusing the vault over one path was the wrong guard against the right hazard.** The
earlier rule saw the danger correctly: quarantine turns "this cannot be read" into "this is
absent", and applied to a revoked permission it would prune rows the store had no evidence
against and call the pruning derived truth. But it answered by refusing the whole heal, so
one unreadable file — a note saved by another user, a mode copied from an archive — left the
entry untrusted, every read refused, and every recovery failing the same way until somebody
found the file. The hazard lives in the prune, not in the heal: a path nothing read is a path
nothing may conclude about. Withholding what was not read, and marking it, keeps every
committed row the earlier rule protected, and gives back the vault it took away. The rule
"refusal is per vault" now means what its examples always meant — the environment as a whole
is broken — and a path's own access is a fact about that path.

**Certification property 1 is the bar: the maintained store equals a build from zero under
the same exclusions.** ADR 0020's third withholder broke it. A directory that becomes a
symbolic link is a root every walk from now on refuses, so a build from zero holds no finding
beneath it — while the maintained store kept every finding that stood there, with nothing
able to reach them again, because the rule read the notation rather than the reason. Where the
refused root's own spelling defeated the address grammar the hold was job-wide over every
marker-carrying place, so one link nobody named froze every undecodable place in the vault,
permanently, in every job. ADR 0023 widened prune authority to read the refusal, against ADR
0020's inversion clause, because the authority was never absent: the walk reads the refusal,
and the refusal is a total statement about everything beneath the entry for as long as the
entry stands.

A broken path is the one place the bar is knowingly short. A build from zero cannot read a
broken document either, so it holds the names-only row and the finding, while a maintained
store keeps the facts it read before the path broke. A build from zero cannot list an
unlistable directory either, so it holds no row beneath it, while a maintained store keeps
every row it held there. Each difference is the store keeping what it knew rather than
discarding it, each is marked by the finding at the path — the directory's finding marking
the gap beneath it — and each ends when the path reads again. Link health reads the gap
the same way: a link spelling a path beneath a marked directory has a target that is unknown,
not absent, and is not judged
([ADR 0044](0044-a-from-zero-build-judges-link-health-once.md)).

## Consequences

**The churn bars lose a row-flip family.** A document edited across the size bound never
flips its row: the row stands and only its finding appears and clears. A document whose
permission is taken away and given back never flips its row either. The remaining row flips
are the three quarantine classes, each a document that genuinely stops being derivable.

**The posture branches on typed document state.** A wholly unread block is a state the text
layer reports — the refusal reason, beside the note it also files — and a diagnostic code
still crosses no seam. Whether an open's failure is one path's or the environment's is
likewise a typed answer at the filesystem seam, matched exhaustively, so an error kind added
later must be assigned to one side rather than inheriting either.

**A degraded row and its finding converge as a pair, at the cost of one indexed findings
lookup per defective document per heal.** A broken document costs one open attempt per heal
that enumerates it and no findings lookup, since its own row says it is owed a write. A
converged vault with no broken path re-derives nothing.

**A broken document's row carries no content hash, and every reader of a hash sees that.**
Deleting a broken document leaves a tombstone that records no hash, as deleting a document
the store never read does, so a repair cannot follow it as a move by its bytes. A repair
looking for the live document that holds a moved file's hash never finds a broken one.

**Answers about a broken document read the row that stands, with the finding beside it.** A
field or tag predicate over a document the heal never read answers from the facts its row
holds — none, for a document the store has never read — and the finding is where "these facts
were not read" is stated, as it is for an unread block. A link that resolves to a document the
store has never read — a row holding no content or structure fact, not merely one holding no
hash — is judged for being broken or ambiguous, which reads names, and not for a missing
anchor, since that document's headings and blocks are unknown rather than absent — as an
anchor on a vault file is not judged
([ADR 0044](0044-a-from-zero-build-judges-link-health-once.md)). A link that resolves to a
document read before it broke is judged against the headings and blocks its row still holds.
A repair plan that edits a readable document over a missing-anchor finding judged that way
proceeds as any repair does, while the write protocol's check of a target's before-state
still refuses a write to the broken path itself.

**A finding whose subject the vault no longer holds is reached by the next walk whose clean
scope covers its place.** The gap ADR 0019 recorded is closed on the subject axis: the stale
finding's end is the ordinary convergence of the walk. The price is the prune-authority
accounting above — a walk that cannot vouch for a place must visibly withhold it, and every
withholder is pinned.

**A scoped leg converges less than the vault heal, and the difference is exactly the
spellings it did not enumerate.** Two shapes fall out of that, both bounded and both ended by
the next heal.

- **A refused root whose own spelling no prefix admits.** The leg has no range of stored
  paths to register, so it takes neither the rows nor the findings at the marker-carrying
  places beneath the root; the heal reaches them by the marker. Until it runs, the maintained
  store holds findings a build from zero does not.
- **The rendered place a refused root stands at**, where the document grammar refuses the
  root's spelling and the directory grammar admits it. Every sibling whose rendering lands on
  that place names it too, so a leg that read one of those spellings leaves the place alone
  and converges the range below the root.

**The two axes travel together but not in one transaction.** Rows die inside the prune's own
increment and the findings are taken once every scope of the job has run. A process killed
between the two leaves rows pruned and findings standing — the state a heal that never pruned
leaves — and the next walk that enumerates those places takes them by this same rule.

**A new skip reason must decide whether it stands.** The reason set is matched exhaustively,
so a reason added later cannot inherit an answer; the seam that converges a dirty path asks
for the answer rather than assuming it.

**The architecture's failure contract changes where it named a path's permissions.** The
permission-loss row of the induced-failure table, which refused at rung 2 and withdrew trust
as a lost watcher on a backend that adds a watch per directory, becomes: the document or
directory is marked broken and skipped, nothing is pruned, the entry reaches `Ready`, and the
path converges when it reads again. The quarantine section's closing rule — refusal stays for
a path whose permissions were revoked, and widening quarantine would turn a broken
environment into silent data loss — narrows to failures of the environment as a whole, and
its list of refusals drops the directory a walk cannot list and the revoked path. The walk's
paging-window bar distinguishes a stat denied for one name, which now withholds and marks,
from the machine's own failures, which still refuse. The signals of pending maintenance at
rest in the rows gain a third: a row holding no content hash is re-derived, which is every
broken document. The wire's health rule, which leaves an anchor on a vault file unjudged,
also leaves unjudged an anchor on a document the store has never read and a path-addressed
link whose spelled path lies beneath a marked directory.

**Inversion.** If partial rows prove untrustworthy in practice — queries answered from a row
whose fields were never read, by callers that never look at the findings beside it — the
revisit is toward quarantine: move the unread-block causes back into the cause class, and
accept that a typo removes a document from the index. If the prune proves untrustworthy —
findings deleted that a walk earned no authority over — the revisit is toward narrower
authority, never toward a stamp: shrink what a walk refusal is read to state before spending a
write per converged document. A refusal class that turns out not to bind a derivation from
zero the same way is withdrawn from the standing set one variant at a time, not by restoring
the notation-level hold. If broken paths hide a failure that is really the environment's —
a mount whose permissions moved wholesale, marking thousands of documents broken — the
revisit is toward reporting it louder, in status and in the heal's own account, never toward
refusing the vault over one path.
