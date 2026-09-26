---
status: accepted
date: 2026-09-26
---

# 0027 — derived indexes split into two lanes, class-scoped link health rides the changeset, and inference never re-enters correctness

Supersedes [ADR 0021](0021-derived-indexes-split-into-two-lanes.md), whose lanes, change
feed, inference firewall and invalidation keys this decision restates whole. ADR 0021 rested
lane 1 on deterministic per-document locality, closed it to cross-document fan-out "at any
price", and kept higher-order derived state off the findings surface. Read as written, those
clauses leave no home for link health: a family of findings whose truth depends on documents
other than the one a link is written in, and which must be exact at every snapshot.
**Class-scoped re-decision of link health rides the lane-1 changeset: the store judges it in
SQL and files its findings inside the changeset, over per-document facts the write already
derives.** This decision replaces ADR 0021's per-document locality with **bounded
locality**, and its cross-document fan-out with **forbidden fan-out**: propagation that is
unbounded or unindexed. The admission is one finding family, under five necessary
conditions and a write-work bar.

## The two lanes

Every unit of derived state beyond the core document projection is an **index projection**:
a declaration naming its inputs — vault-document bytes, or named lane-1 pillars — whether it
is deterministic, and the one key that invalidates it wholesale. The lane is derived from
the declaration, never chosen. A deterministic projection of source bytes is **lane 1**:
written inside the atomic changeset, scoped to the changed set, and covered by the
convergence-to-equivalence bar. The link-health finding family is lane 1 too, by the ruling
below and within its link neighborhood; no other declaration whose inputs reach across
documents is lane 1 without a ruling of its own. Everything else is **lane 2**:
asynchronous, eventually consistent, and maintained by an **engine** — a domain that owns a
sidecar database and derives from committed lane-1 records, never from vault files.

Lane 1 is structurally closed to everything else rather than policed, because it carries
four obligations that all assume deterministic, bounded locality:

- **atomicity** — the changeset is one transaction, scoped to the blast radius of what it
  changes;
- **bounded working memory** — peak memory is a function of the working set, never of the
  vault;
- **incremental equals rebuild** — incremental maintenance equals a from-zero rebuild over
  the same tree;
- **the work bar** — the work a write does follows its blast radius, never the vault's size.

**Forbidden fan-out** is propagation that is unbounded or unindexed: it reaches a set no
bound limits, or it reaches its set other than through an index seek. Unbounded propagation
fails the work bar and holds the changeset's lock and memory past its blast radius;
unindexed propagation fails the work bar. Nondeterminism and model-version dependence fail
incremental equals rebuild. A projection that carries any of them cannot ride the changeset
at any price.

## Link health is re-decided inside the changeset

**Link health** is a validate finding family with three kinds, each a finding about exactly
one link: a **broken** link names no document; an **ambiguous** link names two or more, and
its finding carries a bounded head of five candidates, the exact total, and a hint that names
how to enumerate the rest; a **missing anchor** names one document that lacks the link's
`#heading` or `#^block`. Each finding's truth depends on documents the link is not written
in: which documents are in the class the target names, and what headings and blocks the one
it names holds. Creating, deleting or renaming a document can change a finding about a link
in a document nobody touched.

Five obligations bind these findings. A validate is a pure read over the lane-1 findings
pillar: it runs no rule and derives nothing. Findings are one of the four protected surfaces
behind the correctness boundary
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

**The decision.** The store judges link health in SQL and files those findings inside the
changeset, over per-document facts the host derives from the text layer — link targets,
headings, blocks, and the keys a class is read through. The findings are judged against the
facts the transaction holds once every entry of the changeset is written. The
**re-decided set** is the union of every link held by a document the changeset writes, and
every link whose target keys fall in a class the changeset changes. The exception admits
exactly the link-health family: broken, ambiguous and missing anchor, each a finding about
exactly one link, filed to the findings pillar. It is never an aggregate over the set, and
never a write to another pillar. Within that family, it admits only re-decision that is:

1. **indexed** — every link in the re-decided set is reached by an index seek: a written
   document's links by that document, and a changed class's links by their stored target
   keys;
2. **class-keyed** — every finding it files is keyed by exactly the class keys the discard
   ranges over, so the next change to that class discards it;
3. **deterministic** — the same facts yield the same findings in the same order;
4. **over facts the changeset's own transaction reads** — per-document facts lane 1 derives,
   and no lane-2 or engine output;
5. **bounded by the re-decided set** — its work is the links in that set plus the candidate
   classes they resolve against, with each distinct target key resolved once.

The five conditions are necessary, not sufficient. Backlink counts keyed by class, a rule
that a linked document carries a tag, and tags inherited along links would each meet them;
none is admitted. Any other bounded, indexed cross-document propagation needs its own
ruling.

**The admission carries a write-work bar.** A test holds that the work a write does
re-deciding link health follows the re-decided set and its candidate classes, not the size
of the vault, and it varies candidate multiplicity independently of in-link count, so an
ambiguous finding's exact total cannot hide vault-sized work. The bar is what indexed access
is for: a seek is the only access that keeps the work within the bound, and the bar is what
makes condition 5 an enforced property rather than a promise. The bound is not small in
every vault. A neighborhood can be most of the vault: a hub stem that most documents link
to.

This changes the store's present contract, under which the store discards findings and
never records them, and the host decides which findings an act records. Link-health
findings are the store's to file; every other finding stays the host's.

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

**A link-health finding judged inside the changeset is the one exception.** Its inputs are
read inside the changeset's own transaction, so it is exact at that snapshot, and it is one
finding about one link. What makes it admissible is that exactness, not the lane it is
written in, and the family restriction above bounds it. Every other projection whose inputs
include a model or another index's output stays query-surface-only.

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
`narrowing-arguments-narrow-work`. The anchor rule, judged outside SQL, is an in-memory
post-pass over more rows than the read returns, which fails `no-in-memory-query-layer`. And
maintaining every input while deriving the findings on each read is the exact defect
`derived-findings-are-materialized-and-maintained` names.

## Consequences

**A hub stem costs its in-links, under the write lock.** Creating or deleting a document
whose stem many links name re-decides all of them in that write: adding one `index.md` to a
vault with 5,000 `[[index]]` links re-decides 5,000 links before the changeset commits. The
re-decision runs under the maintainer's write lock, so watcher catch-up and every other
write on that store wait for its duration. That stall is an accepted cost.

**Memory stays bounded while the lock is long.** The re-decided set is judged in the
statement and streamed out of it, never materialized beyond a bounded chunk of rows, so a
hub costs time under the lock and not memory.

**A from-zero heal re-decides common-stem classes once per chunk.** A heal commits in chunks,
and a class touched by many chunks is re-decided by each of them. The heal still converges
to the rebuild's findings; the price is repeated work on common stems.
