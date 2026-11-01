---
status: proposed
date: 2026-11-01
---

# 0047 — derived indexes split into two lanes, link health rides the changeset and a write re-decides only the links whose inputs it moved, a from-zero build judges it once, and inference never re-enters correctness

Supersedes [ADR 0044](0044-a-from-zero-build-judges-link-health-once.md), whose lanes,
change feed, link-health admission, single from-zero judgment, inference firewall and
invalidation keys this decision restates whole. ADR 0044 had every changeset re-decide every
link that reaches a path it writes, "whether or not that class's membership moved", so a write
that fixed a typo in `index.md` re-decided every `[[index]]` link in the vault, inside the
write's transaction on the store's one writer connection. **A changeset re-decides a link in
another document only when it moved an input that link's verdict reads.** A link's verdict
has two inputs from outside the document holding it: the **names** its keys reach, and the
**anchor values** of the one document it names. A write that changes neither re-decides only
the links its own written documents hold. Nothing else changes: the findings every changeset
leaves standing are exactly the findings ADR 0044's set leaves over the same facts, finding
order included, apart from the write generation each was filed at, so the narrowing is a
reduction in work and never in what is found. Link health
stays on the changeset; moving it off was weighed and rejected below. A **from-zero build**
— a vault's first build and every rebuild from zero, the construction
[ADR 0045](0045-a-from-zero-build-writes-once-in-steps.md) builds in steps — still judges link
health once, after every fact has landed, as ADR 0044 ruled and this decision restates.

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
  changes, and so is every other lane-1 transaction that re-decides, as ruled below;
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

**Link health** is a validate finding family with four kinds, each a finding about exactly
one link, and it follows the addressing and eligibility rules every surface already reads a
link's health by:

- a link addressed elsewhere — a scheme link, a network link among them — is not judged and
  raises nothing;
- an eligible link that resolves to no document and no vault file is **broken**, whatever
  extension its last segment carries ([ADR 0040](0040-a-link-resolves-as-obsidian-resolves-it.md));
  a link that resolves to one vault file is healthy, and its anchor is not judged;
- an eligible link that resolves to two or more documents or vault files is **ambiguous**, and its finding
  carries a bounded head of five candidates, the exact total, and a hint that names how to
  enumerate the rest;
- a link that resolves to one document lacking the link's `#heading` or `#^block` has a
  **missing anchor**; a link that resolves to one broken document
  ([ADR 0046](0046-one-unreadable-path-never-breaks-the-vault.md)) is not judged for its
  anchor. Every broken document holds names alone, whether read before or never read, so
  its headings and blocks are unknown rather than absent. An absent body is not an empty
  one: an empty readable document holds a content hash and an empty body, and an anchor on
  a link to it is missing. The predicate tests `body IS NULL` by one primary-key seek per
  judged link and target pair, never a count of zero headings and blocks, which a readable
  document can hold too;
- a path-addressed link beneath a directory marked unreadable
  ([ADR 0046](0046-one-unreadable-path-never-breaks-the-vault.md)) that resolves to no held
  name is not judged: its target is unknown rather than absent. A link resolving to a held
  name resolves as any link to that name does. Its anchor is unjudged where the names-only
  row holds no body. A suffix-addressed link cannot be tied to one directory, so it is judged
  over the names the store holds, and the directory's finding marks the gap those names may have.

A link written through `vault://` that resolves to one candidate carries a **vault-scheme**
finding unless its missing anchor takes precedence.

An embed is a link and carries the same kinds. Every link-health finding has warning
severity.

Each finding's truth depends on documents the link is not written in: which documents its
target names, and what headings and blocks the one it names holds. Creating, deleting,
renaming or moving a document, and editing the headings or blocks of a document a link
names, can each change a finding about a link in a document nobody touched. Its inputs are
lane-1 pillars — stored links, headings, blocks, document keys, and the unreadable-directory
findings the host files — in the same way the tag facet's findings read stored tag rows, so
it is not a projection over another index's output and the inference firewall below applies
to it unchanged.

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

**A finding is discarded by the act that re-decides its link.** Inside its own transaction a
changeset discards every finding standing at a path it writes or kills, every finding filed
under the class key of a path whose names it moved, and every finding keyed by the exact path
of such a path, and then re-decides the links those findings were about beside the links the
re-decided set names. An in-place rewrite that moves an anchor value discards the standing
finding of every link it re-decides for that move: until the anchored-link index is built, by
the same class and exact-path discard a names move runs at the written path, and once it is
built, by each re-decided link's own place. Any other finding stands, because nothing its
verdict reads moved. The class discard predates ADR
0021, which neither admitted nor forbade it; the path-key axis beside it, and the re-decision
that files the findings again in the same act, were ruled by ADR 0027 and are built.

**A link reaches documents through one of two key spaces, and they are not
interchangeable.** A suffix-addressed link — `[[glossary]]`, `[[norn/glossary]]` — is keyed
by the classes its reductions name, and a document is in such a class by its suffix. A
path-addressed link — `[x](./dir/t.md)`, `[[vault://Notes]]` — is keyed by the exact paths
it spells, and no class range reaches it: deleting `dir/t.md` changes the class `t/`, and the
path key `dir/t.md` is not in that range. An unprefixed target in either family, `[x](dir/t.md)`
among them, is suffix-addressed (ADR 0040). A change reaches a link's finding only through the key
space the link is addressed in.

**The decision.** The store judges link health in SQL and files those findings inside the
changeset, over per-document facts the host derives from the text layer — link targets,
headings, blocks, and the keys a link is read through. The anchor rule is a predicate over
readings stored per heading, per block and per link, so the whole judgment is one SQL
predicate. The findings are judged against the facts the transaction holds once every entry
of the changeset is written. The exception admits exactly the link-health family: broken,
ambiguous, missing anchor and vault-scheme, each a finding about exactly one link, filed to the findings
pillar. It is never an aggregate over the set, and never a write to another pillar. A
from-zero build is the one write that judges the same family by the same predicate at
another time, as the next section rules.

**The re-decided set** is the union of:

- every link held by a document the changeset writes;
- every suffix-addressed link whose keys fall in the class of a path whose names the
  changeset moved, as the next section defines;
- every link whose standing finding is filed under such a class — a finding is filed under
  the naming class of each candidate it carries, so a link addressed by path whose candidate
  is named by its minimal disambiguating suffix is reached when a name in that class moves;
- every path-addressed link whose key equals a path whose names the changeset moved,
  including the old path of a rename;
- every anchored link whose keys reach a document whose anchor values an in-place rewrite
  moved, and that names one of the values moved there, as the next section defines — a
  superset of the links resolving to that document alone, which the superset rule admits;
- every path-addressed link whose key lies beneath a directory whose unreadable-directory
  finding the act files or ends, re-decided in the transaction that files or ends that
  finding, so a link left unjudged under a marked directory is judged once it lists again.
  That act first discards the findings already keyed beneath the directory, by a prefix
  range over path keys, and then files again: the equality discard on a written or killed
  path does not reach a key beneath a directory.

## A write re-decides only the links whose inputs it moved

**A link's verdict reads two things outside the document that holds it.** Its broken,
ambiguous and vault-scheme verdicts, and the candidate head and minimal disambiguating
suffixes a finding carries, read the **names** its keys reach: which document and vault-file
rows stand at the classes and exact paths it is keyed by, and in the naming classes of the
candidates it carries; the facts each row is matched by — its path, whether it is a document,
a vault file or a names-only row, its suffix and path keys, and its admitting count, the
fewest target segments that admit it, which the schema's `paths.ambiguity_ignore` globs set;
and which directories along a spelled path are marked unreadable. Its missing-anchor verdict
reads the **anchor values** of the one document it names: the readings and slugs of that
document's headings, the identifiers of its blocks, and whether its body is known at all. The
anchor predicate is an equality over those values — a heading matches a link's anchor text or
marked reading by its reading, or the anchor as written by its slug, and a block matches a
block reference by its identifier — so, while the body stays known, whether a document holds
the place a link names changes only when a value equal to one the link carries enters or
leaves the document. Nothing else outside the holding document enters a verdict: a finding's
message, target, span and ordinal are the holding document's.

**Exactly one kind of entry is narrowed: an in-place rewrite.** An entry is an **in-place
rewrite** where it writes a readable document at a path that held a readable document before
the entry, and the row it writes matches the row it replaces in every fact a row is matched
by — its path byte for byte, its kind, its suffix and path keys, and its admitting count; the
row keeps its identity. A respelling that only a folding comparison calls the same path is a
death and a birth. Every other
entry **moves the names** at every path it reaches and re-decides by them, as ADR 0044 ruled:
a birth, a death, a rename (a death of the old path beside a birth of the new), a vault
file's birth or death, a provisional name written during a from-zero build's names step, a
document that becomes unreadable and keeps its names alone
([ADR 0046](0046-one-unreadable-path-never-breaks-the-vault.md)), a names-only row written as
a readable document, a range beneath an unlistable directory, and a write whose row differs
from the one it replaces in any fact a row is matched by. An entry whose effect on those facts
cannot be read from the entry and the row it replaces is taken to move them. Some of these
leave the names standing and move only the body between known and unknown; re-deciding them
by their names is the superset the next paragraph but one admits, and it reaches every
anchored link the move can flip, including one naming a value neither side holds.

**An in-place rewrite moves the anchor values** it changes: the heading readings, heading
slugs and block identifiers the document holds after the write, set against those it held
before. The comparison reads only the written document's own rows, before the write discards
them and beside the replacement it writes, so its cost is the written document's and never
the vault's. **An in-place rewrite that moves no anchor value re-decides only its own links.**
Its path stays where it stood, every link in another document reads the same inputs before
and after, and that link's standing finding, keyed by classes and paths whose names did not
move, stays: it is exactly the finding a re-decision would file again. The subject discard
still takes every finding standing at the written path, and the re-decision files the
written document's own links again.

**Re-deciding more than the re-decided set files the same findings.** A link whose inputs did
not move is re-decided to the finding it already holds, so a changeset may re-decide a
superset of the set and stay exact; the narrowing is an obligation on the work, which the
write-work bar measures, and never a condition of correctness. **The index that reaches an
anchored link by the anchor value it names is not yet built.** Until it is, an in-place
rewrite that moves an anchor value re-decides every link reaching the written path's class
and exact path, as ADR 0044 did for every write, and the write-work bar holds such a rewrite
to that reach and every other in-place rewrite to its own links. A seek by anchor value alone
across the vault is not that index: a common heading names links to many other documents, and
their work would follow the vault rather than the write.

**The job-end prune's transaction is a lane-1 re-deciding transaction.** Where a walk's
job-end prune ends an unreadable-directory finding
([ADR 0046](0046-one-unreadable-path-never-breaks-the-vault.md)), the transaction that ends
it discards and re-decides the links beneath that directory, under the five conditions
below as a changeset is: the finding's end and the re-decision commit together or not at
all, and the transaction's work is bounded by the re-decided set.

Every link-health finding carries invalidation keys in the key space its link is addressed
in, and the naming classes of the candidates it carries, so the changeset's discard reaches it
through the same keys the re-decided set is found by. Within that family, the exception admits only re-decision that is:

1. **indexed** — every link in the re-decided set is reached by an index seek: a written
   document's links by that document, every other link by its stored keys in its own key
   space — an anchored link reached for a moved anchor value by its keys and that value
   together once the index holding both is built, and until then by its keys alone, as every
   link reaching the written path's class and exact path — a link reached through a
   candidate's naming class by the finding filed under that class, and the links beneath a marked directory by a range seek over path keys under the
   directory's own key. A path-addressed link reaches a marked directory the same way: each
   directory its spelled path passes through is a prefix of that path, and the place key of
   each is sought among the unreadable-directory findings, one seek per directory the spelling
   names, so the work follows the spelling, never the vault;
2. **keyed like its discard** — every finding it files is keyed by exactly the class and path
   keys the discard ranges over, so the next change that moves the names under any of them
   discards it, and the act that re-decides its link for a moved anchor value replaces it;
3. **deterministic, in facts and in order** — the same facts yield the same findings, and the
   order of a path's findings is a function of the findings' facts — the link's position in
   its document, then the kind — never of when they were filed;
4. **over facts the transaction that re-decides reads** — per-document facts lane 1 derives,
   and no lane-2 or engine output;
5. **bounded by the re-decided set** — its work is the links in that set plus the candidates
   they resolve against and the directories their spelled paths name, with each distinct
   target key resolved once.

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
anything link health reads. The walk after a pin discharges it by deriving every row
stamped at or below the pin's generation again, under every schema: the changeset that writes
a document re-decides the links it holds, and a row whose admitting count the new
`paths.ambiguity_ignore` globs change is not an in-place rewrite, so it moves its names and
re-decides every link its class and path reach, including one a chunk already judged against
the old count. The obligation's cost has a floor: every link in the vault is re-decided,
chunked as a heal is. Link health reads only stored per-document facts, so the re-decision
needs no document to be read again; the walk that re-reads every document pays a heal's cost
on every pin as well. A mechanism meeting the floor is not built; schema edits are rare, and
exactness is kept.

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
chunks; a requeued directory that lists again reaches the links beneath it, and its
unreadable-directory finding ends before they are judged, as ADR 0045 rules. The judgment is the one above:
the same SQL predicate, over the same summaries of the keys each link is read through,
filing the same kinds with the same invalidation keys, in the same deterministic order. A
path a filesystem fact requeues while the judgment runs is derived again first, and the
judgment then judges again the links that path reaches — the links it holds, every link its
class and exact path reach, and every link whose finding is filed under its class, as a
changeset that moves that path's names re-decides — before it ends, so every finding it files is judged over the
facts the store holds when it ends. Facts accepted after the judgment ends are reconciled
before `Ready` as ordinary changesets, under the per-changeset rule. The judgment's working
memory is a bounded chunk of links and the candidates they resolve against, plus one count
per distinct key naming more than a candidate head.
The finished store's findings equal the findings per-changeset re-decision files over the
same tree, finding order included, so incremental equals rebuild holds for every store a
reader can adopt, apart from the accepted delay in refreshing a still-broken document's
cause (ADR 0046). Every broken document has the same names-only state in a maintained
store and a build from zero, with an unreadable-document finding. The exception ADR 0046 carves is beneath an
unlistable directory: a maintained store keeps only names, ids and keys beneath it, where a
build from zero holds none. Byte-derived facts and content-side findings are cleared in the
transaction filing the directory's finding. That finding marks the gap until it lists again.

**The four obligations hold for the pass as a from-zero build has them.** Atomicity protects
a reader from a tear between a row and the findings judged over it. A from-zero build admits
readers of the rows it has completed, but it withholds the findings surface, not the store,
until the judgment has run: no reader reads a link-health finding before then, and an open
after a crash before the transaction that ends construction commits discards the
construction whole. Working memory is bounded by the chunk and its candidates, plus one
count per distinct key naming more than a candidate head. The
work bar measures a write against its blast radius, and a from-zero build's blast radius is
the whole vault: judging each link once is the least work that blast radius admits, where
per-changeset re-decision exceeds it.

**A from-zero build may defer judgment because nothing during the build reads the
findings.** A link's broken or ambiguous verdict reads only names — which documents its keys
name, and which directories along a spelled path are marked unreadable — and the vault
schema's `paths.ambiguity_ignore` globs; its missing-anchor verdict reads only the headings
and blocks of the one document it names. The store is first a
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
headings, blocks and the unreadable-directory findings the host files, and the firewall
stands whole over both. A from-zero build's single
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

**Moving link health off the changeset into a deferred drain.** Only the surfaces that
report health read its findings, so a write could record the links it owes a verdict and a
background pass could judge them behind a link-health status, as a from-zero build already
reports one. That takes every cost out of the write, a hub's birth included, but findings
would then be eventually consistent between a write and the drain, which the exactness this
decision keeps forbids, and validate would have to wait on or run the drain. It needs its own
decision and a drain, which belong with a host-wide background queue; a deferred drain would
want this decision's narrow set too, so this decision does not foreclose it.

**Re-deciding every anchored link reaching a document whose anchors moved.** Simpler, and an
admissible superset, but it prices a glossary's new term at every anchored link to the
glossary. The anchor predicate is an equality, so the set of links whose verdict can change is
exactly the links naming a moved value, and that set is what the work is held to.

**Re-deciding per changeset during a from-zero build, as ADR 0027 ruled.** Each chunk
re-judges every class its names touch, against names later chunks have not yet written, and
files findings later chunks discard, at the cost measured above. The repetition buys
nothing, because no reader reads a finding before the last chunk lands.

## Consequences

**A hub stem costs its in-links when its names move, inside the changeset's transaction.**
Creating, deleting or renaming a document whose stem many links name re-decides all of them
in that write: adding one
`index.md` to a vault with 5,000 `[[index]]` links re-decides 5,000 links before the
changeset commits. The re-decision runs inside the changeset's transaction on the store's
one writer connection, so watcher catch-up and every other write on that store wait for its
duration. That stall is an accepted cost.

**Memory must stay bounded while the transaction is long.** The bounded-working-memory
obligation requires the re-decided set to be judged in the statement and streamed out of it,
never materialized beyond a bounded chunk of rows, so that a hub costs time inside the
transaction and not memory; a memory bar is what tests it. A from-zero build's single
judgment is held to the same bar: a bounded chunk of links and the candidates they resolve
against, plus one count per distinct key naming more than a candidate head.

**A full tree heal of an existing store still re-decides common-stem classes once per
chunk.** A heal that maintains an existing store — an attach over a store it adopts, or a
recovery that does not rebuild from zero — commits in chunks under the per-changeset rule,
and a class in which many chunks move names is re-decided by each of them. Such a heal still
converges to the rebuild's findings; the price is repeated work on common stems, largest
when the heal reaches most of the vault.

**Link health reads as still loading until a from-zero build's judgment has run.** Its
findings exist only once the single judgment has run, so every surface that reports them
answers "still loading" until then, for a rebuild of an existing store as for a vault's
first build, as ADR 0045 rules.

**An ordinary edit to a hub costs the hub's own links.** A write that changes a widely linked
document's body — an index, a map of content, a daily note — and leaves its names and anchor
values alone re-decides only the links it holds, however many links name it. A write that
adds, removes or renames a heading or block id there costs, until the anchored-link index is
built, every link reaching its class and path, as every write to it did before; once built,
it costs the links naming a value it moved. A glossary that 2,000 `[[glossary#Term]]` links
name pays for the links naming the term it changed.

**Every in-place rewrite pays a bounded comparison.** A readable document rewritten in place reads its
old heading readings, slugs and block identifiers and compares them with what it writes, a
cost that follows the written document. A write whose names move skips the comparison: it
re-decides by its names regardless.
