---
status: accepted
date: 2026-09-26
---

# 0027 — derived indexes split into two lanes, class-scoped link health rides the changeset, and inference never re-enters correctness

Supersedes [ADR 0021](0021-derived-indexes-split-into-two-lanes.md), whose lanes, change
feed, inference firewall and invalidation keys this decision restates whole. ADR 0021 closed
lane 1 to cross-document fan-out "at any price" and kept higher-order derived state off the
findings surface. Read as written, those two clauses leave no home for link health: a family
of findings whose truth depends on documents other than the one a link is written in, and
which must be exact at every snapshot. **Class-scoped re-decision of link health rides the
lane-1 changeset, judged in SQL over per-document facts the write already derives.** The
admission is a tight exception with five conditions and a write-work bar, and it rests on a
definition ADR 0021 left implicit: **forbidden fan-out is propagation that is unbounded or
unindexed.**

## The two lanes

Every unit of derived state beyond the core document projection is an **index projection**:
a declaration naming its inputs — vault-document bytes, or named lane-1 pillars — whether it
is deterministic, and the one key that invalidates it wholesale. The lane is derived from
the declaration, never chosen. A deterministic projection of source bytes is **lane 1**:
written inside the atomic changeset, scoped to the changed set, and covered by the
convergence-to-equivalence bar. So is the class-scoped re-decision of link health this
decision admits, and only where its declaration meets the five conditions below. Everything
else is **lane 2**: asynchronous, eventually consistent, and maintained by an **engine** — a
domain that owns a sidecar database and derives from committed lane-1 records, never from
vault files.

Lane 1 is structurally closed to everything else rather than policed, because its guarantees
all assume deterministic, bounded locality. The changeset is the unit of atomicity and is
scoped to the blast radius; peak memory is a function of the working set; and incremental
maintenance must equal a from-zero rebuild over the same tree. **Forbidden fan-out** is
propagation from a change that is unbounded, or that reaches what it affects only by a
scan. Forbidden fan-out, nondeterminism, and model-version dependence each break all three
guarantees, so the projection that carries any of them cannot ride the changeset at any
price.

## Link health is re-decided by class inside the changeset

**Link health** is a validate finding family with three kinds: a **broken** link names no
document; an **ambiguous** link names two or more, and its finding carries a bounded head of
five candidates, the total, and a hint that names how to enumerate the rest; a **missing
anchor** names one document that lacks the link's `#heading` or `#^block`. Each finding's
truth depends on documents the link is not written in: which documents are in the class the
target names, and what headings and blocks the one it names holds. Creating, deleting or
renaming a document can change a finding about a link in a document nobody touched.

Five obligations bind these findings. A validate is a pure read over the lane-1
findings pillar: it runs no rule and derives nothing. Findings are a correctness surface
([ADR 0024](0024-search-enhancement-is-a-query-surface-tier.md)), so a link-health finding
must be exact for the snapshot it is read under, not eventually right. The regression
registry states the rest:

- `derived-findings-are-materialized-and-maintained` — "a warm validate reads findings and
  derives nothing";
- `incremental-equals-rebuild` — the maintained index equals a from-scratch rebuild,
  "including finding order";
- `cost-is-independent-of-vault-size` — per-operation cost is a function of "the changed set
  plus its link neighborhood", so the link neighborhood is already priced as part of a
  change's blast radius.

**Class-scoped discard already rides lane 1.** The increment names the class of every path
it changes and, inside its own transaction, discards every finding filed under a class key
that class prefixes. That discard predates ADR 0021, which neither admitted nor forbade it,
and the store's findings contract and this repository's architecture already assume
link-health findings filed under class keys. What was missing is the other half: the
re-decision that files the findings again, in the same act.

**The decision.** The lane-1 changeset re-decides link health for the classes it changes,
judged in SQL over per-document facts the write already derives — link targets, headings,
blocks, and the keys a class is read through. The exception admits only re-decision that is:

1. **indexed** — the links it re-decides are reached by an index seek on their stored target
   keys, never by a scan;
2. **class-keyed** — every finding it files is keyed by exactly the class keys the discard
   ranges over, so the next change to that class discards it;
3. **deterministic** — the same facts yield the same findings in the same order;
4. **over lane-1 facts only** — it reads per-document facts lane 1 derives, and no lane-2
   or engine output;
5. **bounded by the in-links of a changed class** — its work is the links whose targets fall
   in a class the changeset touched, and nothing more.

**The admission carries a write-work bar.** A test holds that the work a write does
re-deciding link health follows the link neighborhood of what it changed, not the size of
the vault. The bar is what makes condition 5 an enforced property rather than a promise.

## Lane 2 reads change through a feed

Lane 2 consumes change through a feed that is a query, not a structure: current document
rows and tombstones, ordered by the store's global write generation, projecting content
fingerprints so a consumer triages before it fetches. Cursors are consumer-owned and valid
within one store epoch; an epoch change means a rescan, and content-addressed sidecar rows
make a rescan recompute only what actually changed, so expensive derived state survives the
cheap state's rebuild. The host relays a contentless wake; a missed wake costs latency,
never correctness. A retained change log with registered subscriber cursors was rejected:
retention pinned by the slowest consumer forces an eviction policy, and eviction forces
consumers to resynchronize from current state — which is what the feed already is. Cross-file
transactional atomicity is not required, because eventual consistency is lane 2's stated
contract.

## Inference never re-enters correctness

Inferred and higher-order derived state — any projection whose inputs include a model or
another index's output — is **query-surface-only**. It never becomes a finding, a plan, or a
repair input. This generalizes the boundary invariant that keeps `norn-embed` blind, and it
is carried the same way: absent crate edges, and a store read surface partitioned so the
correctness path has no API that exposes inferred pillars. A typed advisory finding class
for inferred results was considered and deferred: adding it later is additive, while
withdrawing inferred rows from the table the applier trusts would be nearly impossible.

**Lane-1 re-decision over lane-1 facts is not higher-order derived state in this sense.**
Its inputs are lane-1 facts read inside the changeset's own transaction, so it is exact at
every snapshot and equal to a rebuild by construction. The rule keeps its full force where
it was aimed: a projection over another index's output that is lane-2 or engine output never
becomes a finding.

## Invalidation keys

Every projection names one invalidation key, and the key lives with the state it
invalidates: the vault schema fingerprint for lane-1 schema-keyed tables, an engine's own
configuration and model versions for its sidecar. How invalidated state converges is the
owning side's judgment. Wholesale rebuild is the current implementation and the permanent
always-correct floor; finer responses — adding one index without a rebuild, a pillar
carrying its own version key — are carved evolutions taken when something like measured cost
forces them, not obligations.

## Alternatives

**A lane-2 link-health engine over the change feed.** ADR 0021's firewall forbids it as
written: its findings would be derived from another index's output by an engine. It is also
only eventually consistent, so at any instant between a write and the engine's drain the
findings differ from a rebuild's, which fails `incremental-equals-rebuild`. It needs a
sidecar database, a new crate, and a feed handle wider than the one an engine holds. And a
find that asks which documents hold a finding could not join the sidecar in one snapshot.

**Judging link health at read time, in validate, through SQL joins.** An unnarrowed page
would step every healthy link to find the unhealthy ones, which fails size independence. A
part that drives a read by "holds a finding" would no longer seek what it keeps, which fails
`narrowing-arguments-narrow-work`. The anchor rule, judged outside SQL, is an in-memory post-pass
over more rows than the read returns, which fails `no-in-memory-query-layer`. And
maintaining every input while deriving the findings on each read is the exact defect
`derived-findings-are-materialized-and-maintained` names.

## Consequences

**A hub stem costs its in-links under the write lock.** Creating or deleting a document whose
stem many links name re-decides all of them in that write: adding one `index.md` to a vault
with 5,000 `[[index]]` links re-decides 5,000 links before the changeset commits. The cost is
O(in-links), which is the link neighborhood the size-independence case already prices.

**A from-zero heal re-decides common-stem classes once per chunk.** A heal commits in chunks,
and a class touched by many chunks is re-decided by each of them. The heal still converges
to the rebuild's findings; the price is repeated work on common stems.

**The door admits only what the five conditions admit.** Backlink counts, inherited tags, or
any other derived state computed over other documents' facts are not admitted by this
decision and would need their own ruling.

**A durable "link pass owed" marker is held in reserve.** If heal cost proves too high, the
heal could record that a class's re-decision is owed and run it once, instead of once per
chunk. It is not built now.
