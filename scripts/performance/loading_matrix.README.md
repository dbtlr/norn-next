# Run the loading lab comparison

This is a lab prototype, not a production loading policy.
Non-unique indexes wait for their first admitted reader: finding cleanup and
per-document findings and target resolution after names, field/path Find after
content, anchor and outgoing-link indexes before health, and remaining indexes
by Ready. Production
SQL is restored verbatim. Trigrams are buffered without indexes and bulk
inserted in key order before health; their first reader is ordinary warm work.
The build connection uses `wal_autocheckpoint=10000` and `cache_size=-65536`;
`synchronous` stays at its production value. `--build-cache-size production`
retains the ordinary cache. An integer selects SQLite pages (positive) or KiB
(negative). `--no-deferred-indexes` keeps secondary indexes throughout the lab
build. Both controls apply only to four-step arms and are retained in each cell.
Ready restores the original cache and WAL settings.
Requests flush the current transaction at the next host chunk boundary, then
run content and structure in one transaction of their own. Each cell starts one
fresh worker, serially, with release optimization, `induced-failure`, 128-document
changesets, and 128-document host chunks grouped into SQLite transactions of
at most 2,048 documents. No cell restarts. The
controller, fixture generation, and audits are outside the worker's clocks.
Filesystem caches remain uncontrolled. A discarded warmup is not a claim that every arm has identical cache state.

## Prepare builds and frozen fixtures

Run from the harness checkout after committing Rust changes. Keep `TMPDIR` short
for the filesystem socket tests. All outputs stay inside this checkout.

```bash
mkdir -p .t
export TMPDIR="$PWD/.t" CARGO_TARGET_DIR="$PWD/.t/target"
python3 scripts/performance/loading_matrix.py --prepare-builds "$PWD/.t/r9-builds"
python3 scripts/performance/loading_matrix.py --config .t/r9-builds/config.json --freeze-fixtures .t/r9-frozen --sizes 314 10014
```

Commit the Rust implementation before `--prepare-builds`. Each build
runs `rustup run 1.97.1 cargo test --locked --release` with `induced-failure`.
The checkout-local `.t` directory holds generated artifacts and is excluded
from source receipts. Keep source files outside `.t`.

One `--prepare-builds` invocation creates all three roles: main worker
`perf/norn-459-main-worker`, the committed prototype HEAD, and health-once
`e944a338`. Main receives the current `performance/public.rs` harness overlay
for the timed Get. The main receipt records its commit and verifies that only the
performance Rust worker differs from product baseline `2e213c09`. Every role has its own
checkout, target directory, frozen binary, and build receipt. Compiler identities
must match. Source and binary hashes, features, profile, Cargo configuration, and
prototype Rust source equality are checked again before and after the matrix.
Python-only follow-up commits can reuse receipts. A repeated preparation can
reuse detached sources and role-specific Cargo caches. Main may retain the exact
current harness overlay. Other source edits are refused. Preparation rebuilds and
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
both `link_resolution_loading` and `link_health_loading`. Early linked timings
remain in the side-by-side group.

## Run serial pilot and 10014 cells

Run these commands sequentially after preparing builds and fixtures. The pilot
uses one discarded main warmup followed by the second Williams row. Its accepted
four-step cell seeds main in the same run. The 10014 sequence has exactly three
cells: four-step/rebuild, main at that observed request offset, then
four-step/batched. No extra seed or 10014 warmup runs.

```bash
python3 scripts/performance/loading_matrix.py --config .t/r9-builds/config.json --fixtures .t/r9-frozen/fixtures --sizes 314 --repetitions 1 --row-start 1 --fts-mode rebuild --warm-write-repetitions 10 --output .t/r9-pilot
python3 scripts/performance/loading_matrix.py --config .t/r9-builds/config.json --fixtures .t/r9-frozen/fixtures --sizes 10014 --repetitions 1 --arms four-step --fts-mode rebuild --skip-warmup --warm-write-repetitions 10 --output .t/r9-10014-rebuild
python3 scripts/performance/loading_matrix.py --config .t/r9-builds/config.json --fixtures .t/r9-frozen/fixtures --sizes 10014 --repetitions 1 --arms main --fts-mode rebuild --mid-request-reference .t/r9-10014-rebuild --skip-warmup --warm-write-repetitions 10 --output .t/r9-10014-main
python3 scripts/performance/loading_matrix.py --config .t/r9-builds/config.json --fixtures .t/r9-frozen/fixtures --sizes 10014 --repetitions 1 --arms four-step --fts-mode batched --skip-warmup --warm-write-repetitions 10 --output .t/r9-10014-batched
```

`--row-start 1` selects the zero-based second Williams row: four-step, no-body,
main, health-once. The discarded main warmup remains first. Arm filtering keeps
relative order. Main uses the latest accepted preceding four-step request time
for its size, unless an explicit offset overrides it.

`--fts-mode` supplies lab-only `NORN_PERFORMANCE_FTS_MODE=rebuild|batched`.
It changes no production defaults. The no-body arm remains available with both
modes. A main reference must be a completed, gated four-step run with matching
FTS mode, frozen fixture hash, and target. `--mid-request-offset-ns N` can instead
supply an observed worker-clock request offset for one size. Main results record
both the requested offset and actual request time, so scheduling delay remains
visible. Main's concurrent request does not wait for caller prefix ACKs.

Use `--skip-warmup` for the specified three-cell 10014 sequence and follow-on
invocations in the same serial session.
The default still retains one discarded main warmup per size. A larger matrix
can use a completed rebuild run as `--mid-request-reference`; default Williams
rows and repetition counts remain unchanged. Each invocation needs a new output
directory and retains all host-admission and correctness gates.

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
cells. An unfiltered 314 row has five. Warmups pass the gates but never enter summaries.
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
before canonical reads and all warm operations. Main and four-step cells also run
the prototype worker's store-owned `verify` command at both audit phases. Each
phase requires one positive native integrity receipt, including FTS integrity.
`result.integrity` and phase-specific stdout/stderr logs retain that evidence.
Health-once retains its archived audit contract. Another audit runs after warm writes
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
`page_count`, `page_size`, database file size, WAL file size, and exact ordered
`sqlite_schema` rows. Schema text is descriptive evidence, separate from the
mandatory semantic and path-to-ID gates. Post-Ready
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
- Canonical Get, Find, and Search after Ready, plus linked and mid-target Gets.
- Layout at both audit phases, and mandatory gates.

Not comparable (side by side, no ratios):

- Four-step `ready` (end of step 5, full-text complete) versus public Ready.
- `first_get`, `first_find`, `first_search`: lab hooks and public paths have
  different admission paths and findings guarantees.
- Lab parent clocks and lab resources.
- Step boundaries, busy times, pull-ahead, commits, and full-text rows.
- Mid-request, own structure, delivery, and request-to-delivery times.
- Health-once timings as a judge-once effect; its archived pre-382 base is context.
- Identity, page-map, and construction digests.

Every result retains raw endpoints and responses. Parent clocks start before
`Current` construction; worker clocks start at the worker's `Instant`; step
report boundaries start at lab construction. Never add these clocks. Console
public Ready uses only `public_ready.worker_ns`; parent delivery is explicitly
not comparable.

Lab callbacks return typed answers during construction and buffer stdout until
the build ends. Parent receipt times and resource samples describe post-build
transport delivery. `delivery_ns` equals the actual `query_finished_ns` on the
worker clock. `query_started_ns` and `query_host_ns` record query cost.
`earliest_available_ns` records the observed prerequisite, never an estimated
answer time. Queries delay subsequent steps, so their cost remains visible.

Both `small.md` and linked `large.md` are requested at startup.
`first_get_targets` reports each target's observed own-step-3 structure time and
actual hook delivery. Get has no global-content prerequisite. Early lab Get uses
public target resolution and admits only its completed callback document.
`--lab-get-targets small bench/large.md` runs the same targets as the main worker;
the default targets remain rooted paths. Before global content completion, a
provisional ambiguity reports loading: pending content may quarantine a name.
A target outside its completed callback also reports loading. Outgoing resolution SQL waits until
relations complete. Individual field projections use the document's primary key
before global content completion. Declared keys and keys carried by the document
can answer then. An absent undeclared key still waits for the final field
universe, including unknown-key suggestions. The small-document callback also
records and validates a `status` projection at its own step 3. Early records
carry `link_resolution_loading=true` and `link_health_loading=true`. Their body,
fields, headings, blocks, tags, and authored links are checked against frozen
sources and final public answers. Incomplete link resolutions and health are
explicitly excluded from those completeness claims. Post-Ready linked Get also
checks the broken-link finding.

The mid target is at 90% of sorted document paths, outside the first 128-path
batch and distinct from startup requests. The worker schedules intake at the first
content `Batch` with `completed * 4 >= total`. Content counts document read
attempts. A separate thread waits 1 ms after that callback before queueing the request,
while the next chunk runs. Request-to-delivery includes the remaining chunk
work, the forced group commit, and requested-document derivation.
Worker request JSON and `mid_request` label this asynchronous scope explicitly.
`mid_request` records request, own structure, actual delivery, and latency on
one worker clock. The host report's `build_request_ns`,
`build_requested_structure_ns`, and `build_delivery_ns` maps use the separate
build origin. `build_request_to_delivery_ns` subtracts only within that origin.
Never add worker and build clocks.

First field Find runs at content completion. Lexical Search and Ready wait for
full-text completion. Boundary, busy, and step-counter maps contain `names`,
`content`, `structure`, `relations`, and `full-text`. Step 5 reports
`fts_rows_inserted`. Busy time excludes observer queries and requested-path
pull-ahead. It measures elapsed time, not CPU. Boundary intervals include those
pauses. Relations includes the authority check. Full-text includes FTS and
finish teardown, restoration of production connection settings, and a TRUNCATE
WAL checkpoint. Structure timestamps share the build transaction, so
`timing_commits` is zero. timing_ns remains a subset of busy time.
`Structured` and GetReady run after the group commits. The build report's
`requested_structure_ns` records derivation completion before that commit.
`build_commits` counts nonempty grouped transactions. The step counters include setup,
standalone index-boundary, and finish commits.
`profiles` records inclusive calls and nanoseconds from norn_db::profiling
for each pass: commits, index builds, row writes by fact table, FTS, checkpoint,
read/hash, parse/derive, and SQL. Nested spans overlap; do not sum them.
`refused_ns` and `request_refusals` retain explicit request refusals. A refusal
stops the measurement worker because all selected targets must be readable.
SQLite shadow-table work remains uncounted. Ordinary job evidence exposes
changesets rather than committed transaction counts.

Main retains its ordered small Get, linked Get, and field Find prefix. Its mid
Get uses the same target and the rebuild cell's observed wall offset. It reports
actual request and public response times, including unavailable responses.
Lab and main mid results are **not comparable (side by side, no ratios)**.
Health-once executes no early queries. Its first answers use public paths after
Ready. No earlier endpoint is inferred.

The default warm workload has eight reads and four writes.
`--warm-write-repetitions 10` repeats set_field and rename_hub ten times each,
retaining one create and one subject rename. `warm_write_medians` reports host
and round-trip medians separately for those two write kinds. Write readbacks are
separate history entries outside each operation clock. No aggregate warm score
can hide an individual regression. Archived health-once uses its own transport,
audit adapter, and exactly-once validator in a separate Python process. The
final audited link count equals the initial count because these writes preserve
link cardinality.

Python checks use the repository runner:

```bash
TMPDIR="$PWD/.t" python3 -m unittest discover -s scripts/performance -v
TMPDIR="$PWD/.t" python3 -m unittest discover -s scripts/performance/live_mode -v
```

The findings-order test compares ordinary and lab construction with a shared
128-document bound on case-sensitive vaults, the measured Linux configuration.
A separate folded-path singleton comparison kills removal of the order-restoring
sort. Ordinary folded-path multi-document changesets file in byte order; lab
chunks retain folded caller order. Their finding ID sequence is not asserted
equivalent. This construction-order difference does not change semantic state.
