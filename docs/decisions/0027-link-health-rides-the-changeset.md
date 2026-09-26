---
status: accepted
date: 2026-09-26
---

# 0027 — derived indexes split into two lanes, link health rides the changeset, and inference never re-enters correctness

Supersedes [ADR 0021](0021-derived-indexes-split-into-two-lanes.md), whose lanes, change
feed, inference firewall and invalidation keys this decision restates whole. ADR 0021 rested
lane 1 on deterministic per-document locality and closed it to cross-document fan-out "at any
price". Read as written, that leaves no home for link health: a family of findings whose
truth depends on documents other than the one a link is written in, and which must be exact
at every snapshot. **The re-decision of link health rides the lane-1 changeset: the store
judges it in SQL and files its findings inside the changeset, over per-document facts the
write already derives.** This decision replaces ADR 0021's per-document locality with
**bounded locality**, and its cross-document fan-out with **forbidden fan-out**: propagation
that is unbounded or unindexed. The exception it makes is to the closure against fan-out,
not to the inference firewall, which stands whole. It admits one finding family, under five
necessary conditions and a write-work bar.

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

Lane 1 is closed to everything else by ruling rather than by case-by-case review: what may
ride the changeset is decided once, per family, and the bars test that what rides it keeps
lane 1's four obligations, which all assume deterministic, bounded locality:

- **atomicity** — the changeset is one transaction, scoped to the blast radius of what it
  changes;
- **bounded working memory** — peak memory is a function of the working set, never of the
  vault;
- **incremental equals rebuild** — incremental maintenance equals a from-zero rebuild over
  the same tree;
- **the work bar** — the work a write does follows its blast radius, never the vault's size.

**Forbidden fan-out** is propagation that is unbounded or unindexed: it reaches a set no
bound limits, or it reaches its set other than through an index seek. Unbounded propagation
fails the work bar and holds the changeset's transaction and memory past its blast radius;
unindexed propagation fails the work bar. Nondeterminism and model-version dependence fail
incremental equals rebuild. A projection that carries any of them cannot ride the changeset
at any price.

## Link health is re-decided inside the changeset

**Link health** is a validate finding family with three kinds, each a finding about exactly
one link, and it follows the addressing and eligibility rules every surface already reads a
link's health by:

- a link addressed elsewhere — a scheme link, a network link among them — is not judged and
  raises nothing;
- a link whose target names an attachment and resolves to no document is not judged and
  raises nothing;
- an eligible link that resolves to no document is **broken**;
- an eligible link that resolves to two or more documents is **ambiguous**, and its finding
  carries a bounded head of five candidates, the exact total, and a hint that names how to
  enumerate the rest;
- a link that resolves to one document lacking the link's `#heading` or `#^block` has a
  **missing anchor**.

An embed is a link and carries the same kinds. Every link-health finding has warning
severity.

Each finding's truth depends on documents the link is not written in: which documents its
target names, and what headings and blocks the one it names holds. Creating, deleting,
renaming or moving a document, and editing the headings or blocks of a document a link
names, can each change a finding about a link in a document nobody touched. Its inputs are
lane-1 pillars — stored links, headings, blocks and document keys — in the same way the tag
facet's findings read stored tag rows, so it is not a projection over another index's output
and the inference firewall below applies to it unchanged.

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
link-health findings filed under class keys. Two things are not built. The re-decision that
files the findings again, in the same act, is one. A path-key axis is the other: a finding
today can carry only class keys, which end in a separator, so neither the storage nor the
discard can yet key a finding by the exact path a path-addressed link spells.

**A link reaches documents through one of two key spaces, and they are not
interchangeable.** A suffix-addressed link — `[[glossary]]`, `[[norn/glossary]]` — is keyed
by the classes its reductions name, and a document is in such a class by its suffix. A
path-addressed link — `[x](dir/t.md)`, `[[vault://Notes]]` — is keyed by the exact paths it
spells, and no class range reaches it: deleting `dir/t.md` changes the class `t/`, and
`dir/t.md` is not in that range. A change reaches a link's finding only through the key
space the link is addressed in.

**The decision.** The store judges link health in SQL and files those findings inside the
changeset, over per-document facts the host derives from the text layer — link targets,
headings, blocks, and the keys a link is read through. The anchor rule is a predicate over
readings stored per heading, per block and per link, so the whole judgment is one SQL
predicate. The findings are judged against the facts the transaction holds once every entry
of the changeset is written. The exception admits exactly the link-health family: broken,
ambiguous and missing anchor, each a finding about exactly one link, filed to the findings
pillar. It is never an aggregate over the set, and never a write to another pillar.

**The re-decided set** is the union of:

- every link held by a document the changeset writes;
- every suffix-addressed link whose keys fall in the class of a path the changeset writes or
  kills, whether or not that class's membership moved;
- every path-addressed link whose key equals a path the changeset writes or kills, including
  the old path of a rename.

Every link-health finding carries invalidation keys in the key space its link is addressed
in, so the changeset's discard reaches it through the same keys the re-decided set is found
by. The path half of that discard, and the path keys it ranges over, are part of what this
decision rules and does not yet exist. Within that family, the exception admits only re-decision that is:

1. **indexed** — every link in the re-decided set is reached by an index seek: a written
   document's links by that document, and every other link by its stored keys in its own key
   space;
2. **keyed like its discard** — every finding it files is keyed by exactly the class and path
   keys the discard ranges over, so the next change to any of them discards it;
3. **deterministic, in facts and in order** — the same facts yield the same findings, and the
   order of a path's findings is a function of the findings' facts — the link's position in
   its document, then the kind — never of when they were filed;
4. **over facts the changeset's own transaction reads** — per-document facts lane 1 derives,
   and no lane-2 or engine output;
5. **bounded by the re-decided set** — its work is the links in that set plus the candidates
   they resolve against, with each distinct target key resolved once.

The five conditions are necessary, not sufficient. Backlink counts keyed by class, a rule
that a linked document carries a tag, and tags inherited along links would each meet them;
none is admitted. Any other bounded, indexed cross-document propagation needs its own
ruling.

**The admission carries a write-work bar.** A test holds that the work a write does
re-deciding link health follows the re-decided set and the candidates it resolves against,
not the size of the vault, and it varies candidate multiplicity independently of in-link
count, so an ambiguous finding's exact total cannot hide vault-sized work. The bar is what
indexed access is for: a seek is the only access that keeps the work within the bound, and
the bar is what makes condition 5 an enforced property rather than a promise. The bound is
not small in every vault. A neighborhood can be most of the vault: a hub stem that most
documents link to.

**A schema change that changes what link health reads owes the vault a link-health
re-decision.** The ambiguity-ignore globs decide class membership without any document being
written, and a schema pin discards every finding keyed by the fingerprint it replaced. The
existing re-derivation does not cover this: a pin re-derives unchanged rows only where the
schema declares a reporting tag vocabulary or a field whose type does not order as text,
and a pin under a vault declaring neither re-derives no row. That predicate's own rule is
that a declaration gaining a consumer joins it in the same change, so the change that lands
link health carries this obligation.

**The store's contract changes at one point.** Today the store records only the findings the
host hands it with a changeset, and decides none. Link-health findings are the first it
judges and files itself; every other finding stays the host's to decide.

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

A projection over named lane-1 pillars is not a projection over another index's output: the
tag facet's findings read stored tag rows and link-health findings read stored links,
headings and blocks, and the firewall stands whole over both.

## Invalidation keys

Every projection names one invalidation key, and the key lives with the state it
invalidates: the vault schema fingerprint for lane-1 schema-keyed tables, an engine's own
configuration and model versions for its sidecar. How invalidated state converges is the
owning side's judgment. Wholesale rebuild is the current implementation and the permanent
always-correct floor; finer responses — adding one index without a rebuild, a pillar
carrying its own version key — are carved evolutions taken when something like measured cost
forces them, not obligations.

## Alternatives

**A lane-2 link-health engine over the change feed.** The inference firewall forbids it: a
finding filed from an engine's sidecar is a projection over another index's output. It is
also only eventually consistent, so at any instant between a write and the engine's drain
the findings differ from a rebuild's, which fails `incremental-equals-rebuild`. It needs a
sidecar database, a new crate, and a feed handle wider than the one an engine holds. And a
find that asks which documents hold a finding could not join the sidecar in one snapshot.

**Judging link health at read time, in validate, through SQL joins.** An unnarrowed page
would step every healthy link to find the unhealthy ones, which fails size independence. A
part that drives a read by "holds a finding" would no longer seek what it keeps, which fails
`narrowing-arguments-narrow-work`. An anchor rule judged per read would either join every
link to its target's headings on every page, which is the unnarrowed cost again, or run as
an in-memory post-pass over more rows than the read returns, which fails
`no-in-memory-query-layer`. And maintaining every input while deriving the findings on each
read is the exact defect `derived-findings-are-materialized-and-maintained` names.

## Consequences

**A hub stem costs its in-links, inside the changeset's transaction.** Creating or deleting a
document whose stem many links name re-decides all of them in that write: adding one
`index.md` to a vault with 5,000 `[[index]]` links re-decides 5,000 links before the
changeset commits. The re-decision runs inside the changeset's transaction on the store's
one writer connection, so watcher catch-up and every other write on that store wait for its
duration. That stall is an accepted cost.

**Memory must stay bounded while the transaction is long.** The bounded-working-memory
obligation requires the re-decided set to be judged in the statement and streamed out of it,
never materialized beyond a bounded chunk of rows, so that a hub costs time inside the
transaction and not memory; a memory bar is what tests it.

**A from-zero heal re-decides common-stem classes once per chunk.** A heal commits in chunks,
and a class touched by many chunks is re-decided by each of them. The heal still converges
to the rebuild's findings; the price is repeated work on common stems.
