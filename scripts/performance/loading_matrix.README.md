# Run the loading lab comparison

This is a lab prototype, not a production loading policy. Each cell starts one
fresh worker, serially, with release optimization, `induced-failure`, 128-document
changesets, and 128-document four-step transactions. No cell restarts. The
controller, fixture generation, and audits are outside the worker's clocks.
Filesystem caches remain uncontrolled. The single discarded warmup per size is
not a claim that every arm has identical cache state.

## Prepare builds and frozen fixtures

Run from the harness checkout after committing Rust changes. Keep `TMPDIR` short
for the filesystem socket tests. All outputs stay inside this checkout.

```bash
mkdir -p target/t
export TMPDIR="$PWD/target/t" CARGO_TARGET_DIR="$PWD/target"
python3 scripts/performance/loading_matrix.py --prepare-builds "$PWD/target/loading-builds"
python3 scripts/performance/loading_matrix.py --prepare-plan --sizes 2014 10014 > target/loading-plan.json
python3 scripts/performance/loading_matrix.py --config target/loading-builds/config.json --freeze-fixtures target/frozen --sizes 314 2014 10014
```

One `--prepare-builds` invocation creates all three roles: main worker
`perf/norn-459-main-worker`, the committed prototype HEAD, and health-once
`e944a338`. The main receipt records its commit and verifies that only the
performance Rust worker differs from product baseline `2e213c09`. Every role has its own
checkout, target directory, frozen binary, and build receipt. Compiler identities
must match. Source and binary hashes, features, profile, Cargo configuration, and
prototype Rust source equality are checked again before and after the matrix.
Python-only follow-up commits can reuse receipts. A repeated preparation can
reuse clean detached sources and role-specific Cargo caches; it rebuilds and
reissues every receipt in one invocation.

`--freeze-fixtures` generates each background size exactly once before timing.
To reuse a base fixture parent, add `--fixtures PATH`; its subdirectories use
background counts (`300`, `2000`, `10000`). A reuse invocation requires every
selected size to exist in that parent. Separate invocations with disjoint sizes
can populate the same destination. The existing n2014 base digest must be
`3c0fa350593b337a953d14c59dcb240ea5afca93d36d120d6c7b38fc5fd3635a`, and
its generator and probe authoring must still match main. Every reused tree is
rehashed against its manifest. Fixture manifests retain `base_fixture_sha256`.

The version-2 matrix fixture extends `bench/large.md` with `[[small#Details]]`,
`[Small](small.md#Details)`, and `[[matrix-missing-target]]`. It retains fourteen
probe documents, keeps `small.md` unchanged, and records a new `tree_sha256`.
Re-freezing an already enriched base is refused. The base digest and the
measurement digest are deliberately distinct. Timed
runs require `--fixtures` and reject fixtures without this version. All selected
fixtures are copied and verified before any timed cell, including the warmup.

The main controller supplies `NORN_PERFORMANCE_PUBLIC_GET_TARGETS` as an ordered
JSON list: `small`, then `bench/large.md`. The worker calls `Host::get` for each
target with the existing clocks and caller ACK protocol, then calls `Host::find`.
Without this control, the worker retains its original small Get and Find prefix.
Main linked answers include link-health findings; lab linked answers carry
health loading. Both early linked timings remain in the side-by-side group.

## Run a pilot or the full matrix

```bash
python3 scripts/performance/loading_matrix.py --config target/loading-builds/config.json --fixtures target/frozen/fixtures --sizes 314 --repetitions 1 --output target/loading-pilot
python3 scripts/performance/loading_matrix.py --config target/loading-builds/config.json --fixtures target/frozen/fixtures --sizes 2014 --repetitions 1 --arms health-once four-step --output target/loading-key-pilot
python3 scripts/performance/loading_matrix.py --config target/loading-builds/config.json --fixtures target/frozen/fixtures --sizes 2014 10014 --output target/loading-matrix
```

A repetition is one four-cell Williams row. The rows cycle in this order:

```text
main              four-step          health-once        four-step-no-body
four-step         four-step-no-body  main               health-once
four-step-no-body health-once        four-step          main
health-once       main               four-step-no-body  four-step
```

Defaults are eight repetitions at 2014 (two squares), four at 10014 (one square),
and one at 314. Each size also has one main cell labelled `warmup-discarded`.
`--repetitions N` overrides the row count for every selected size. Arm filtering
retains relative Williams order and the warmup. The default full plan has 50
cells. The 314 pilot has five. Warmups pass the gates but never enter summaries.
Use a new output directory for every run.

Immediately before every cell the controller calls `sync`. It then records the
raw load average and all fields of `/proc/pressure/{cpu,io,memory}`. Admission
requires load1 per effective CPU at most 0.5, CPU `some avg10` at most 10%, IO
`some avg10` at most 5%, and memory `some avg10` at most 1%. Effective CPU count
is the minimum of process affinity and every quota on the process's cgroup-v2
ancestry, resolved from `/proc/self/cgroup`.
The controller waits at most 300 seconds, sampling every five seconds, then
refuses the cell. Each `host-admission.json` records thresholds, samples, reasons,
acceptance, and elapsed wait. A refused cell leaves `run.json` incomplete.

## Gates and layout

An independent, untimed store-owned audit runs immediately after public Ready,
before canonical reads and all warm operations. Another runs after warm writes
and worker shutdown, against the final source tree. This catches step-4 errors
before `rename_hub` can re-judge links. Both audit phases check source hashes and
semantic state. Native provenance replay checks every retained post-Ready
response against the raw worker stream and final generation.

At each audit phase, a read-only SQLite connection reads exact `SELECT path,id`
pairs from `documents` and `vault_files`. Semantic digests AND these path-to-id
maps must equal the first accepted reference for the same frozen fixture at
both phases. References persist in `FIXTURES/loading-gate-reference.json`, or
`--gate-reference PATH`. Never delete that file to make a mismatch pass. These
fixtures contain no undecodable names. Identity, construction, and page-map
hashes remain descriptive; exact path-to-id equality is a separate mandatory
gate. On any mismatch the controller fails loudly and records the failed run.

`layout.post_ready` and `layout.post_workload` capture every `dbstat` table and
index, including SQLite and FTS internal objects: pages, payload bytes, unused
bytes, overflow pages, and page bytes. They also capture `freelist_count`,
`page_count`, `page_size`, database file size, and WAL file size. Post-Ready
inspection runs while the idle worker remains open; SQLite reads the committed
WAL view. File size alone does not describe the full live store. The retained
`documents_layout` field additionally describes post-workload B-tree fill and
physical fragmentation; its scope is only the documents table.

## Read the report

`summary.json` separates the following categories. Each numeric metric reports
its median and min–max across retained repetitions. These are descriptive
statistics with no significance tests. No ratios are calculated.

Comparable (main vs four-step vs four-step-no-body; health-once context only):

- Public Ready on the worker clock, with link health complete.
- Peak RSS (`VmHWM`) and end CPU for the worker process.
- Document opens, documents derived, links judged, keys resolved, candidates
  read, and findings written.
- Each of twelve warm operations individually, with `host_ns` and round trip.
- Canonical Get, Find, and Search after Ready, plus the linked whole Get.
- Layout at both audit phases, and mandatory gates.

Not comparable (side by side, no ratios):

- Four-step `ready` (end of step 3) versus public Ready.
- `first_get`, `first_find`, `first_search`: lab hooks and public paths have
  different admission paths and findings guarantees.
- Lab parent clocks and lab resources.
- Step boundaries, busy times, pull-ahead, and commits.
- Health-once timings as a judge-once effect; its archived pre-382 base is context.
- Identity, page-map, and construction digests.

Every result retains raw endpoints and responses. Parent clocks start before
`Current` construction; worker clocks start at the worker's `Instant`; step
report boundaries start at lab construction. Never add these clocks. Console
public Ready uses only `public_ready.worker_ns`; parent delivery is explicitly
not comparable.

Lab callbacks buffer typed answers and emit them after construction. Their
parent receipt times and resources describe post-build delivery. Each lab query
reports `earliest_available_ns`, actual `query_started_ns`, `query_finished_ns`,
and `query_host_ns`. The earliest-answer estimate is availability plus measured
builder-query duration; it is not an actual earlier execution. Each query's
builder time appears beside Ready because queries delay subsequent steps.

Both `small.md` and the linked `large.md` are requested in four-step arms.
`first_get_targets` reports each separately, with its own observed step-3 instant
beside admitted Get. Admission waits for that structure AND vault-wide content,
so the difference shows the vault-wide step-2 wait. Early Get is labelled
"health loading". It returns exact content and structure, with no step-4
findings. The whole linked Get checks both link families, an anchor, and a
broken target. The post-Ready answer checks its broken-link finding.

Pinned main exposes one early Get (`small`) and Find. Its second whole Get
uses the public path after Ready and explicitly says so. Health-once executes
no early queries; all its first answers use the public path after Ready. No
earlier endpoint is inferred. Main's public Find retains
`query_started_ns`: it starts after the controller acknowledges Get.

Relations boundary and busy time include `four_step::finish` teardown and the
authority check. Busy time excludes observer queries and requested-path
pull-ahead; it is elapsed time, not CPU. Boundary intervals include those pauses.
SQLite FTS trigger work is not counted as explicit rows or commits. Ordinary
job evidence supplies changesets rather than committed transaction counts.

The fixed warm workload has eight reads and four writes. Write readbacks are
separate history entries outside each operation clock. No aggregate warm score
can hide an individual regression. Archived health-once uses its own transport,
audit adapter, and exactly-once validator in a separate Python process. The
final audited link count equals the initial count because these writes preserve
link cardinality.

Python checks use the repository runner:

```bash
TMPDIR="$PWD/t" python3 -m unittest discover -s scripts/performance -v
TMPDIR="$PWD/t" python3 -m unittest discover -s scripts/performance/live_mode -v
```

The findings-order test compares ordinary and lab construction with a shared
128-document bound on case-sensitive vaults, the measured Linux configuration.
A separate folded-path singleton comparison kills removal of the order-restoring
sort. Ordinary folded-path multi-document changesets file in byte order; lab
chunks retain folded caller order. Their finding ID sequence is not asserted
equivalent. This construction-order difference does not change semantic state.
