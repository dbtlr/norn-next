---
status: proposed
date: 2026-10-26
---

# 0044 — derived indexes split into two lanes, link health rides the changeset, a from-zero build judges it once, and inference never re-enters correctness

Supersedes [ADR 0027](0027-link-health-rides-the-changeset.md), whose lanes, change feed,
link-health admission, inference firewall and invalidation keys this decision restates
whole. ADR 0027 had every changeset re-decide the links it reaches, a from-zero build's
included, and priced the repetition that follows: a build commits in chunks, names arrive a
chunk at a time, and a class touched by many chunks is re-judged by each of them. **A
from-zero build judges link health once, after every fact has landed, rather than per
changeset.** A **from-zero build** constructs a canonical store from empty — a vault's first
build and every rebuild from zero — and is the same construction
[ADR 0045](0045-a-from-zero-build-writes-once-in-steps.md) builds in steps, under the same term.
No reader can adopt that store as complete until its construction ends, no reader reads a
link-health finding before the judgment has run, and no query's answer or write's safety
reads one, so nothing needs the findings before every fact they are judged over exists.
Every changeset after the judgment — the reconcile that precedes `Ready` included — keeps
ADR 0027's per-changeset re-decision unchanged, and the store a from-zero build finishes
holds exactly the findings per-changeset re-decision files over the same tree.

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
  **missing anchor**; a link that resolves to one document the store has never read — a
  broken document ([ADR 0046](0046-one-unreadable-path-never-breaks-the-vault.md)) that holds
  only the row its path derives — is not judged for its anchor, because that document's
  headings and blocks are unknown rather than absent.

An embed is a link and carries the same kinds. Every link-health finding has warning
severity.

Each finding's truth depends on documents the link is not written in: which documents its
target names, and what headings and blocks the one it names holds. Creating, deleting,
renaming or moving a document, and editing the headings or blocks of a document a link
names, can each change a finding about a link in a document nobody touched. Its inputs are
lane-1 pillars — stored links, headings, blocks and document keys — in the same way the tag
facet's findings read stored tag rows, so it is not a projection over another index's output
and the inference firewall below applies to it unchanged.

Five requirements bind these findings. A validate is a pure read over the lane-1 findings
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
pillar. It is never an aggregate over the set, and never a write to another pillar. A
from-zero build is the one write that judges the same family by the same predicate at
another time, as the next section rules.

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

**Every schema pin owes the vault a link-health re-decision over every link.** Link-health
findings are keyed by the vault schema fingerprint, as every finding is, and a pin discards
every finding keyed by the fingerprint it replaced — whether or not the schema changed
anything link health reads. The existing re-derivation does not cover this: a pin re-derives
unchanged rows only where the schema declares a reporting tag vocabulary or a field whose
type does not order as text, so a vault holding a broken link whose schema edit adds one
text-typed field would lose that finding while a rebuild files it. The mechanism that
discharges the obligation is left open, and its cost has a floor: every link in the vault
is re-decided, chunked as a heal is. Link health reads only stored per-document facts, so
the re-decision needs no document to be read again; a mechanism that re-reads every
document to reach it pays a heal's cost on every pin as well. Schema edits are rare, and
exactness is kept. This obligation is not yet built.

**The store's contract changes at one point.** Before ADR 0027 the store recorded only the
findings the host handed it with a changeset, and decided none. Link-health findings are the
first it judges and files itself; every other finding stays the host's to decide.

## A from-zero build judges link health once

**The scope is every from-zero build.** A from-zero build constructs a canonical store from
empty: a vault's first build, and every rebuild of an existing store from zero — over a DDL
fingerprint, derivation version or path order the store does not match, over damage, or by
any other rung that discards the database and derives the vault again. No reader can adopt
the store as complete until its construction ends: the store carries an under-construction
mark until then, and an open that finds the mark rebuilds from zero rather than resuming
over what a crash left, as ADR 0045 rules. A full tree heal of an existing store, a recovery
that does not rebuild from zero and every write after the build ends maintain an existing
store; each keeps the per-changeset re-decision above unchanged. A schema pin keeps the
obligation ruled above, whose mechanism stays open; that mechanism may reuse the single
judgment's routine.

**During a from-zero build no changeset re-decides link health.** Once the structure facts
of every document — its headings, blocks and links — have landed, the store judges every
eligible link once, plus the links a requeued path reaches, in path order, in bounded
chunks. The judgment is the one above:
the same SQL predicate, over the same summaries of the keys each link is read through,
filing the same kinds with the same invalidation keys, in the same deterministic order. A
path a filesystem fact requeues while the judgment runs is derived again first, and the
judgment then judges again the links that path reaches — the set a changeset writing or
killing that path re-decides — before it ends, so every finding it files is judged over the
facts the store holds when it ends. Facts accepted after the judgment ends are reconciled
before `Ready` as ordinary changesets, under the per-changeset rule. The judgment's working
memory is a bounded chunk of links and the candidates they resolve against, never the vault.
The finished store's findings equal the findings per-changeset re-decision files over the
same tree, finding order included, so incremental equals rebuild holds for every store a
reader can adopt.

**The four obligations hold for the pass as a from-zero build has them.** Atomicity protects
a reader from a tear between a row and the findings judged over it. A from-zero build admits
readers of the rows it has completed, but it withholds the findings surface, not the store,
until the judgment has run: no reader reads a link-health finding before then, and an open
after a crash before the transaction that ends construction commits discards the
construction whole. Working memory is bounded by the chunk. The
work bar measures a write against its blast radius, and a from-zero build's blast radius is
the whole vault: judging each link once is the least work that blast radius admits, where
per-changeset re-decision exceeds it.

**A from-zero build may defer judgment because nothing during the build reads the
findings.** A link's broken or ambiguous verdict reads only names — which documents its keys
name — and the vault schema's `paths.ambiguity_ignore` globs; its missing-anchor verdict
reads only the headings and blocks of the one document it names. The store is first a
faithful copy of the files, and link-health findings serve validate and repair. Stored
link-health findings are read only by the surfaces that report them — validate, a find's
`HasFinding` part and a findings column — and by repair, which plans from them; no other
query's answer reads them, and no write's safety does. A write other than a repair judges
the health of the links it changes itself while it plans, and resolution reads names and
stored link rows, never a finding. A reader that meets a from-zero build before its judgment
has run sees link health as still loading, never as absent, and a `HasFinding` part is
refused as not ready, as ADR 0045 rules.

**What it buys.** Measured on a lab prototype in release builds, on a 10,014-document
fixture, per-changeset re-decision during a from-zero build made 89,186 link judgments for
59,969 links and wrote 32,874 link-health findings, most of which later changesets
discarded. In a phase profile of that build alone, link health was about 9.3 s of about
23.6 s; that profile measures construction, not the end-to-end time to `Ready` ADR 0045
reports for a build that also served early reads. Judging once over the same vault,
with no fact accepted during the build, made exactly 59,969 judgments, one per link, and wrote
4,001 findings, the findings the finished store holds.

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
headings and blocks, and the firewall stands whole over both. A from-zero build's single
judgment reads the same pillars and nothing else.

## Invalidation keys

Every projection names one invalidation key, and the key lives with the state it
invalidates: the vault schema fingerprint for lane-1 schema-keyed tables, an engine's own
configuration and model versions for its sidecar. Link health's findings carry that
fingerprint, which any pin moves, and beside it the class and path keys the changeset's
discard ranges over for the re-decided set. A finding a from-zero build files carries the
same keys, so the first write after the judgment discards it exactly as it would discard a
finding an ordinary changeset filed. How invalidated state converges is the owning side's judgment.
Wholesale rebuild is the current implementation and the permanent always-correct floor;
finer responses — adding one index without a rebuild, a pillar carrying its own version
key — are carved evolutions taken when something like measured cost forces them, not
obligations.

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

**Re-deciding per changeset during a from-zero build, as ADR 0027 ruled.** Each chunk
re-judges every class its names touch, against names later chunks have not yet written, and
files findings later chunks discard, at the cost measured above. The repetition buys
nothing, because no reader reads a finding before the last chunk lands.

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
transaction and not memory; a memory bar is what tests it. A from-zero build's single
judgment is held to the same bar.

**A full tree heal of an existing store still re-decides common-stem classes once per
chunk.** A heal that maintains an existing store — an attach over a store it adopts, or a
recovery that does not rebuild from zero — commits in chunks under the per-changeset rule,
and a class touched by many chunks is re-decided by each of them. Such a heal still
converges to the rebuild's findings; the price is repeated work on common stems, largest
when the heal reaches most of the vault.

**Link health reads as still loading until a from-zero build's judgment has run.** Its
findings exist only once the single judgment has run, so every surface that reports them
answers "still loading" until then, for a rebuild of an existing store as for a vault's
first build, as ADR 0045 rules.

**Warm writes still re-decide by ADR 0027's set.** A write that changes no name, heading or
block still re-decides every link the set names for the paths it writes. Narrowing that set
for such writes is a separate decision, not taken here.
