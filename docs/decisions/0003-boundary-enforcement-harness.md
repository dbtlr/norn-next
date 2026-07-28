# 0003 — the boundary contracts are enforced by machinery, and the mapping from invariant to mechanism is code

The crate map states a dependency allowlist and thirteen boundary
invariants. A contract stated and not checked is a contract that decays
at the speed of whatever compiles, so this decision is about what checks
them: an architecture gate over the dependency graph, a lint ruleset for
the rules the graph cannot express, and a mapping from each invariant to
what carries it. **The mapping is code rather than prose**, because
enforcement mechanics become true by being executable and a second,
unexecuted spelling of them would drift from the first the moment either
moved.

Not everything lands at once, and the split is deliberate: a bar whose
subject does not exist yet cannot be written honestly. **The machinery
lands here; the bars land with their subjects.**

## The contract

**The gate asserts equality against the allowlist, restricted to the
crates the workspace has earned.** Three claims, each of which fails the
gate on its own:

- Every observed workspace normal-dependency edge is in the allowlist.
  An edge that is not there fails whether or not it points somewhere
  sensible, because the table is exhaustive and a new edge is a
  deliberate edit to it.
- Every allowlist edge whose two endpoints are both present is observed.
- Every workspace member's name is one of the crates the map names. A
  crate cannot join the workspace without an edit to the map.
- No member depends on a local crate the workspace does not hold as a
  member, and no directory under `crates/` holds a manifest the member
  set omits. A crate outside the member set is outside the allowlist,
  outside the lint ruleset and outside every test the workspace runs, so
  excluding one is the cheapest way to make every gate here stop
  applying.

**The transcription is checked against the document.** The allowlist and
the crate list live twice — as tables in `architecture.md` and as data in
the gate — and the gate parses both tables out of the document and
asserts set equality, along with the invariant numbering. The reader
demands the exact column shapes and at least one row, so a change to the
document's table shape fails the parse rather than silently reading
nothing.

The restriction to present crates is what makes the gate binding today
rather than one release from now. The map describes the whole target
shape and the workspace holds only what has been earned, so a naive
exact equality would fail on the first day for the most ordinary reason
imaginable: `norn` (bin) has no `norn-serve` edge because `norn-serve`
does not exist. Restricting equality to present endpoints keeps the
rejection direction total — an unpermitted edge always fails — while
leaving an unearned crate's edges neither required nor violated.

**The required direction has a cost, and it is paid rather than
avoided.** An allowlist edge between two crates that both exist has to
be real. That is what makes the allowlist a description of the workspace
instead of a wish about it, and it means a permitted edge is earned by
the code that needs it: the size-independence pair names its two scales
as generator profiles, which is what makes `norn-testkit → norn-fixtures`
an edge rather than a line in a table.

**Development-dependency edges are read and discarded; build-dependency
edges between members are rejected.** Dev edges cycle by construction —
every crate's tests reach the testkit — so gating them would gate a cycle
the design intends. A build-dependency edge is a different matter: the
allowlist is a set of normal edges, nothing in the crate map earns a
build script reaching another member, and rejecting what the contract
does not describe is the reading that keeps the table exhaustive.

**The graph is read under a pinned matrix, and judged across it.**
`cargo metadata` reports a *resolved* graph, so an `optional = true`
dependency behind an off-by-default feature is simply absent from it. The
matrix therefore names two selections — the default resolution and
`--all-features` — and the readings are judged together, asymmetrically:

- An edge outside the allowlist fails if **any** selection shows it. A
  violation behind a feature flag is still a violation.
- A permitted edge whose endpoints are both present is satisfied if
  **any** selection shows it. An edge that exists only when a feature is
  on is still an edge, and demanding it under every selection would
  forbid a permitted edge from ever being optional.
- The unknown-crate, build-dependency and local-crate checks run under
  **every** selection, because each is a failure on its own terms.

A feature no selection turns on fails the gate as a coverage gap:
`--all-features` satisfies that by construction, and the check is what
makes narrowing the matrix later a visible act rather than a quiet one. A
target-specific dependency adds a platform filter to the same table.

**Lint rules arrive one at a time, as each invariant's subject becomes
real.** Of the five symbol-level rules the crate map names, two have
subjects today and are configured: `std::fs` disallowed workspace-wide,
and no direct stdout writes outside the render seam. The other three —
no SQLite connection outside `norn-store`, no `serde_json::Value`
crossing the wire seam, no `norn-config` registry-surface use outside
`norn-host` — name crates that do not exist, and a rule configured
against nothing is a rule nobody has tested. They are carried as pending
entries in the mapping instead, so their arrival is a deliberate edit
rather than a discovery.

The configuration is one workspace-root `clippy.toml` because that file
does not merge per-crate. The dev crates are therefore inside the rule
rather than exempt from it, and they carve out what they legitimately do
— writing temp trees, reading corpus files — with an `#[allow]` **at the
use site**, on the smallest item that holds the effect. **The allow is
the audit marker**: it says where an effect lives, which is the whole
reason a crate-wide allow is the wrong shape.

The filesystem rule covers every spelling of the effect, not one module's
worth: the calls spelled as `Path` methods, the ones spelled through the
unix extension traits, and `set_current_dir`, which relocates every
relative path in the process at once. `path.exists()` is
`std::fs::metadata` under another name, and a rule that stops at a module
boundary states a restriction that any autocomplete gets around.

**The ruleset binds std-surface effects, and that is its whole reach.**
An effect reached another way is outside it and review-held: a child
process inheriting this process's stdout writes to a terminal no render
seam saw, and a raw `libc` call reaches the filesystem without naming a
`std` path. Naming the boundary is what keeps a green clippy run from
being read as a claim it does not make.

**The mapping from invariant to mechanism is executable data, and it is
checked in both directions.** An invariant carries one or more
mechanisms: an *edge-held* one names the crates the allowlist must give
no path between; a *lint-held* one names a rule from the ruleset, and a
live rule's required entries must be configured; a *review-held* one
names the judgment a person makes, which is the honest form of a gap
rather than a formality. The reverse direction matters as much: every
entry configured in `clippy.toml` must belong to some live rule in the
mapping — matched on the `(key, path prefix)` pair, since a rule that
owns a prefix under one clippy key does not own it under another — and
every rule in the ruleset must be named by some invariant. A lint cannot
arrive in one place and not the other.

**Edge-held absence is reachability, not adjacency.** "`norn-embed`
cannot reach findings" is not satisfied by there being no direct
`norn-embed → norn-store` edge: two allowlist edits that each look
harmless compose into a path, and a type reached through one crate is
reached. The claims are evaluated over the transitive closure of the
allowlist, and a failure names the route.

Two entries in that table are worth stating because the obvious reading
of them is wrong. **Invariant 2 is not edge-held**: `norn-testkit →
norn-store` is a permitted edge, so no absence of edges can say "no
crate outside the store opens a connection" — the lint scoped to the
crates that ship says it, and the sole-applier half is a judgment made
with invariant 4. **Invariant 8's filesystem half has no edge holding it
either**: `norn-client` has machine-local filesystem effects and no
`norn-fs` edge, so the filesystem rule and its use-site allows are the
whole of what carries it.

**Assertion machinery is helpers, and it opens no database.** Counter
snapshots, the pairing of emitted SQL with its query plan, and the
size-independence pair are all data-shaped: they are handed readings and
compare them. The testkit takes no SQLite dependency and opens no
connection, which is invariant 2 holding for the harness itself. What is
missing is the subject — the read builders and the derivation counters
that arrive with `norn-store` — and the bars are written where that
subject lives.

The plan pairing deserves its own sentence, because it is the one place
the machinery enforces something rather than assisting. A plan assertion
is only worth anything against the statement the builder emitted, so the
type holds the statement and the plan rows together and every assertion
is a method on the pair. There is no way to assert about a plan without
naming the SQL it came from. The pairing earns its keep a second way:
SQLite reports a plan in terms of the statement's *aliases*, so
`documents AS d` becomes `SCAN d`, and only the statement says what `d`
is.

**The scan bar is about unboundedness, not about indexlessness.** A
covering-index scan visits one index entry per row of the table, which is
the cost the bar exists to forbid, so it fails the bar even though it
names an index. The weaker bar — no read of a table's own rows without an
index — is a separate assertion. Three plan forms are not table reads and
fail neither: a virtual table consulted through its own index
specification (how an FTS5 `MATCH` is planned, and unbounded when that
specification is empty), an inline `VALUES` clause, and a scan of a
co-routine or materialized subquery, whose own steps are judged as their
own rows.

**A comparison that measured nothing fails.** Two counter readings must
carry the same counter names, so a counter renamed or dropped between
them is a difference rather than a zero; and a size-independence pair
fails when either side recorded no counters, or when every counter is
zero on both sides. Agreement between two runs that never reached their
instrument is not evidence about size.

**A process the harness spawns runs under isolation, and the isolation
is the harness's own contract.** Each run gets a fresh private directory;
`HOME`, `TMPDIR` and the XDG variables point inside it; the environment
is built from an allowlist rather than inherited wholesale, with `PATH`
the one variable carried over. Nothing mutable is shared between
concurrent runs. **A build artifact is copied to a private path before it
is executed**, because the artifact is a file a concurrent build is
entitled to rewrite and executing a file mid-rewrite is a failure of the
harness rather than of the program. A run's environment is exactly the
allowlist plus `PATH`, and that is asserted rather than described.

Peak resident set is read from the kernel's own accounting when the child
is waited on, rather than sampled; the units differ by platform and the
conversion is in one place. **The measurement covers the spawned child
and every descendant that child waited on** — that is what `wait4`
accounts for — so a subject that forks a worker and leaves it running
measures nothing about it. **The spawn path is unix-only**: the
wait-and-account call is `wait4` and the status decoding is the unix one.
The child is reaped exactly once, on the success path by that call and on
every failure path by a kill and a wait, so a run that failed to be
measured leaves no process behind. Each output stream is read up to a
limit and the outcome says whether what it holds is a prefix: a child
printing without bound is a subject under test, not a reason for the
process measuring it to die.

## Consequences

- An edge that would let a boundary violation compile cannot be added
  quietly. It is either in the allowlist — a diff somebody read — or the
  gate is red.
- A crate cannot join the workspace by being convenient. Its name has to
  be in the crate map first — and it cannot leave by being excluded
  either, because a directory under `crates/` that the member set omits
  is a gate failure.
- The tables in `architecture.md` and the data in the gate cannot drift.
  Editing one without the other is red.
- A permitted edge between two present crates has to be real, so the
  allowlist describes the workspace rather than describing an intention.
  Removing the last use of such an edge is a gate failure until the table
  says so too.
- Adding a lint entry without claiming it in the mapping fails, and
  claiming a live rule without configuring it fails. The two files cannot
  drift apart in either direction.
- Three of the five named rules do not run yet, and the mapping says so
  in a form that a test reads. That is a smaller lie than a rule
  configured against a crate that does not exist.
- The review-held invariants — one obvious path, surface-specific
  parameters, the fs event stream, one parser — are named as review-held.
  Nothing enforces them, and the record says nothing enforces them.
- Every filesystem and stdout effect in the workspace reached through a
  `std` path is findable by grepping for its allow. That is the audit the
  carve-outs buy, and its edge is the ruleset's reach: an effect reached
  through `libc` or inherited by a child process is not in it.
- A permitted edge may be optional, and a forbidden one may not hide
  behind a feature. Both follow from judging the matrix as a whole rather
  than selection by selection.
- The gate reads the workspace once per selection, so its cost is linear
  in the size of the matrix. Adding a selection is adding a `cargo
  metadata` run.
- A suite that spawns a process gets isolation without asking for it, so
  a run cannot pick up the developer's config directory, and two
  concurrent runs cannot meet in a cache. It gets it on unix only.

## Evidence

- Mimir **NORN-a1** (the decisions taken before this repository existed)
  and **NORN-a4** (the crate-boundary pass) are where the invariant spine
  and the crate map were derived. [`architecture.md`](../architecture.md)
  is their authoritative form, and it is the table this gate transcribes.
- `docs/architecture.md` states that the authoritative mapping of
  invariant to mechanism is the harness's code and not that document.
  This decision is the other half of that sentence: the mapping is in
  `crates/norn-testkit/src/invariants.rs`, and the checks that keep it
  consistent with the allowlist and with `clippy.toml` are beside it.
- The gate reads `cargo metadata` through `serde_json`, which the testkit
  already depends on, rather than through a crate that models cargo's
  output. The reading is a dozen fields deep and adding a dependency to a
  gate whose whole subject is dependencies is a cost with no return.
- The failure modes are unit-tested against synthetic graphs — an
  unpermitted edge, a missing required edge, an unknown member, a
  build-dependency edge, an excluded local crate, an edge only one
  selection shows — because a gate that has only ever been seen passing
  is a gate nobody has watched work.
- The plan predicates are unit-tested against detail strings SQLite
  itself emits, not against strings written from memory: a covering-index
  scan, an aliased scan and search, a virtual-table index scan, a `VALUES`
  clause, a co-routine, an automatic covering index. A predicate over a
  format is only as good as the samples of the format it was tried on.
