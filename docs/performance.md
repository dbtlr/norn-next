# Measure the runtime baseline

The baseline runs twelve search, get, and write workloads through the current
production host and an isolated legacy daemon. It preserves raw observations,
known-answer fixtures, executable identity, and a self-contained HTML report.

These measurements describe the selected workflows. They are not acceptance
thresholds, correctness oracles, CLI measurements, or long-duration leak tests.
Reads that meet a from-zero build use production early admission under
[ADR 0048](decisions/0048-a-build-step-ends-at-its-closing-take.md). The baseline uses an
opt-in integration-test worker, which adds no product dependency or API.

## Build and run

Use Linux, Python 3.11 or newer, Rustup, and an installed legacy Norn 0.48.3
binary. The resource collector reads the subject process's own `/proc` entries.
Choose an output directory outside the repository, with a short absolute path
so the private Unix socket fits the platform limit. Keep other builds and
benchmarks idle while collecting timings.
The build helper also requires its target directory, receipt and receipt log
paths to resolve outside the repository. It rejects an in-tree path before
creating outputs, so generated artifacts cannot change the source digest.

Set the output home and the actual legacy executable, avoiding a version-manager shim:

```sh
PERF_HOME="$HOME/norn-performance"
LEGACY_NORN="/path/to/legacy/norn"
```

Build with the compiler pinned by `rust-toolchain.toml`:

```sh
python3 scripts/performance/provenance.py \
  --target-directory "$PERF_HOME/build" \
  --receipt "$PERF_HOME/current-build.json"
```

The receipt binds the executable hash to Cargo's declared optimization profile,
compiler version, source identity, and build command. The build removes
environment overrides for Cargo profiles and explicitly pins the compiler,
disables wrappers, and sets the highest-precedence Rust flags to empty. The runner rejects a
changed executable or a profile other than optimization level 3 with debug
assertions disabled. The receipt hashes all nonignored source files before and
after compilation. A changed source tree requires a new receipt before running.
Cargo configuration files, including files outside the repository, are recorded
by hash and checked again before execution. Build and incremental environment
overrides are removed. Builds have a 30-minute timeout and stream diagnostics to
the receipt's `.cargo.jsonl` and `.cargo.log` siblings, preserving partial output
on failure. Metadata commands have a 30-second timeout.
The report distinguishes executable source identity from runner identity and
states whether the build checkout was dirty. A test target retains its development
dependency features; the receipt records them separately from a shipping binary claim.

Run the first baseline:

```sh
python3 scripts/performance/baseline.py \
  --current-build "$PERF_HOME/current-build.json" \
  --legacy "$LEGACY_NORN" \
  --output "$PERF_HOME/run-001"
python3 scripts/performance/report.py "$PERF_HOME/run-001"
```

The default scales contain 300, 2,000, and 10,000 background documents, plus
fourteen probes. Each operation has five warmups and 25 measured calls in each
of two fresh process runs. Engines run serially, with their order reversed in
the second run. Use `--sizes`, `--samples`, `--warmups`, and `--repetitions` to
change the matrix. The realistic generator requires at least 300 documents in
this workload.

Reuse the exact first-run fixtures after building another current revision:

```sh
python3 scripts/performance/baseline.py \
  --current-build "$PERF_HOME/current-build.json" \
  --legacy "$LEGACY_NORN" \
  --fixtures "$PERF_HOME/run-001/fixtures" \
  --output "$PERF_HOME/run-002"
python3 scripts/performance/report.py "$PERF_HOME/run-002"
```

Add `--loading operator-prefix-public` to take the public first Get and first
Find on the current worker's caller thread while it attaches. This mode needs a
worker built with the `induced-failure` feature: build it with
`provenance.py --startup-profile`, and pass that build's receipt as
`--current-build`. A worker built without the feature refuses the mode. The
legacy daemon starts as usual. The worker writes both answers to
`loading-checkpoints.json` in each run's work directory. The runner checks each
served answer against the fixture: an early Get may read link resolution, link
health and findings as still loading, and every other part must match exactly.
Each answer's
`first_answers` sample records `query_host_ns`, its disposition and where it
was read from. `early_step` names the build step that admitted an early answer,
and `canonical` marks a canonical read. The sample also records the pull-ahead
evidence the build had spent by then.

The runner checks the frozen tree digest before copying it. This avoids treating
a generator revision as a runtime change. Output directories must be new;
partial and failed runs remain available for diagnosis. Report generation
refuses incomplete or missing samples. No schedule is activated by these commands.

## Workloads

| Workload | Independently checked result |
| --- | --- |
| Rare text | Exactly one planted document |
| Common text | 25 unique matching documents from the complete candidate set |
| Missing text | No documents |
| Filtered text | All fourteen documents under `bench/` |
| Metadata query | First 25 active documents under each implementation's path order |
| Small get | Complete body of about 2 KiB and its two frontmatter fields |
| Large get | Complete body of about 48 KiB and its two frontmatter fields |
| Section get | The known `Details` section within the large document |
| Field update | A new value each time, confirmed by a get and an independent file check |
| Create | A fresh explicit path, exact authored bytes, and immediate get |
| Rename | Alternating paths with no backlinks; old path absent, destination bytes preserved, and new get succeeds |
| Hub rename | Alternating paths with ten backlink rewrites, checked on disk |

The background uses `norn-fixtures`' realistic shape at seed 7, with document
count varied independently and symlinks disabled. The controller plants unique
query tokens and metadata partitions. Probe documents use explicit paths and
an empty schema. Legacy's unknown-field advisories therefore remain part of its
write response; neither implementation enforces additional schema constraints.
Creates add the same bounded number of documents to each engine's run.

## Measurement boundaries

- The current side uses `Host<ProductionEntryOps>` through a persistent JSONL
  worker. It reuses the existing attachment helper: 128-row/chunk bounds, one
  lifecycle worker, native watching, and 50 ms watcher polling.
- The legacy side uses its warm Unix-socket MCP daemon. Dedicated XDG cache,
  configuration, data, and state directories isolate its socket, store, audit
  log, and background sweep. The daemon identity must match the spawned PID.
  `HOME` stays unchanged. Inherited `NORN_` variables are removed from both
  subjects; only runner-issued root/token values and the explicit legacy
  `NORN_CACHE_CHANNEL=dev` selection are supplied. No live vault or existing daemon is contacted.
- Round-trip latency includes the request, server work, response serialization,
  transport, and controller decoding. Current host-only latency stops before
  response serialization and is retained as a separate diagnostic.
- Legacy MCP includes text and structured representations of its response;
  current uses a development envelope with counters. These transport costs
  differ. The legacy binary hash and daemon build ID identify the installed
  application, but its original compiler and build flags remain unknown.
- Text queries use isolated whole words with known candidate sets. Legacy
  performs substring matching, path sorting, and an exact count. Current uses
  ranked lexical search and bounded pages. Equal candidates do not imply equal
  ranking, features, or internal work. The worker explicitly selects lexical
  search, so vector/fusion candidate-depth limits do not apply. Unexpected
  current read advisories fail the run; each sample preserves advisory data.
- Get probes check complete bodies and frontmatter. Additional facets differ.
  Legacy section responses include the heading line and document context;
  current returns heading metadata and section body separately.
- Write timing ends at the applied reply. A separate write-plus-readback
  measurement includes the confirming get. Filesystem assertions are outside
  those clocks. A partial write, stale readback, or missing backlink fails the run.
  The controller reads set/move source bytes before timing; these are warm-file
  workloads on both engines. Creates and backlink rewrites are checked as bytes,
  without newline normalization.
- Startup ends at the first validated common-text query, because a protocol
  hello can precede lazy indexing. Every process starts with fresh derived
  state. OS filesystem caches are uncontrolled; this is not cold-disk latency.
  Current host setup is recorded separately. It includes case detection,
  construction, and acquisition of the shared test-watcher lease, which can
  wait behind another test.
- RSS and peak RSS come from the subject's own `VmRSS` and `VmHWM`, avoiding
  inherited parent high-water readings. They include native allocations and
  allocator retention. The generator, controller, and legacy child sweeps are
  excluded. Short-run RSS growth does not establish a memory leak.
- SQLite counters describe statement work but do not count every operation
  inside FTS virtual tables. Pair their size curves with clock measurements.
- Warmups remain in the raw evidence but are excluded from distributions.
  Outliers are retained. p95 is descriptive at the selected sample count;
  report differences need repeated qualification before an optimization claim.

## Evidence and checks

`run.json` records identity, environment, completion, and any failure.
`binaries/` holds the exact executables used, preventing later builds or installs
from changing a running comparison or removing its reproduction inputs.
`samples.jsonl` records every warmup, measured call, resource reading, and
verification outcome. `fixtures/` holds immutable sources and manifests.
`work/` holds each independent copy, example requests and responses, and daemon
logs. `summary.json` records distributions, per-process medians, and counters.
`report.html` is self-contained and contains no local filesystem paths.

## Attribute startup cost

The startup experiment reuses the frozen corpus and known-answer reads. It runs
each fresh process through its first validated query, closes it, then restarts
over the same derived store. No writes are part of this experiment. The second
start is a **cache-warmed existing-index restart**: the intervening equivalence
audit reads every source document and the derived store. A cold-cache restart
is a separate experiment.

```sh
python3 scripts/performance/provenance.py \
  --target-directory "$PERF_HOME/build" \
  --receipt "$PERF_HOME/startup-standard.json"
python3 scripts/performance/provenance.py \
  --startup-profile \
  --target-directory "$PERF_HOME/build" \
  --receipt "$PERF_HOME/startup-build.json"
python3 scripts/performance/startup.py \
  --current-build "$PERF_HOME/startup-build.json" \
  --control-build "$PERF_HOME/startup-standard.json" \
  --fixtures "$PERF_HOME/run-001/fixtures" \
  --output "$PERF_HOME/startup-001" \
  --batches 128 512 1024 --repetitions 3 --timings both
python3 scripts/performance/startup_report.py "$PERF_HOME/startup-001"
```

The profiling build enables the existing `induced-failure` harness feature.
Its timing recorder is disabled until the worker explicitly arms it. Use
`--timings both` to compare timing on and off with the same executable, or
`--timings off` for a clock-disabled control. `--control-build` additionally
interleaves a standard build from the same source without the host feature,
separating feature and job-tail costs from timing-recorder overhead. Inherited fault environment
variables are removed by the adapter. These builds are measurement subjects,
not shipping-binary performance claims.
Both worker variants retain the store/database development dependency features;
the standard variant omits the host's additional feature. Compare the
observed-Ready column for a common boundary across those two builds.

To compare revisions, build a standard worker in the baseline checkout with
that checkout's `provenance.py`. Add both reference arguments to the startup
command, keeping the candidate's `--control-build`:

```sh
python3 scripts/performance/startup.py \
  --current-build "$PERF_HOME/startup-build.json" \
  --control-build "$PERF_HOME/startup-standard.json" \
  --baseline-build "$PERF_HOME/baseline-standard.json" \
  --baseline-source "$BASELINE_SOURCE" \
  --fixtures "$PERF_HOME/run-001/fixtures" \
  --output "$PERF_HOME/revision-001" \
  --batches 128 --repetitions 4 --timings both
```

The baseline receipt must match its own checkout and executable. Both source
trees must remain unchanged until measurement ends. The report names every
build's source and executable hashes. Compare `baseline` with `standard` for
revision changes; compare the candidate's profile variants to assess timing
and feature overhead. Use an even repetition count to balance variant order.
Every variant uses the same frozen source fixture and
persisted-state audit. A baseline with a different store schema needs a
compatible audit before this runner can compare it.

The experiment varies changeset size while keeping the store page at 128 rows.
Existing suites retain their original policy. Each configuration runs serially;
size and batch order rotate between repetitions. Record CPU and main-process RSS
alongside the elapsed clocks. The worker waits for the startup job's bookkeeping
to finish after observing `Ready`, and reports both boundaries separately:
`attach_ns` and `observed_ready_ns` end at observed readiness;
`startup_complete_ns` additionally includes the feature build's bookkeeping wait.
The worker and the host test readiness helper poll every millisecond. Readiness
probes read the entry's published state under its gate; they do not query the
store. Other bounded test waits retain adaptive backoff up to 50 milliseconds.

Timers report inclusive elapsed time and call counts. Parent and child times
must not be added together. The top-level breakdown partitions document planning,
row and index writes, link-health maintenance, commit, other increment work and
remaining startup time. Document-row timing includes parameter preparation and
SQLite triggers, including FTS; it does not isolate FTS alone. Watch installation
and synchronization are caller intervals, and background watcher work can overlap
them. The worker rejects unknown or unfinished timing evidence through the runner.
Nested link-health timings separate candidate resolution, link reads, finding
discards, and finding writes. Preparation scopes distinguish compiling or
retrieving SQL statements from executing them. The finding metadata scope
includes its generation update and schema-fingerprint read.

A fresh profiled build must read each document once. The runner rejects a cell
whose `document_opens` differs from the document count plus one, the single read
of the vault's declaration.

After the first validated query, the runner records database, WAL, and shared
memory file lengths while the worker remains open. These readings are outside
the startup clock. They are file lengths at that boundary, not peak disk
usage or allocated filesystem blocks.
Clean shutdown is timed separately, including process exit and controller
cleanup. The validated lifecycle runs from process launch through all
known-answer reads and shutdown. The persisted-state audit follows that clock.
Report both alongside startup when a change can defer checkpoint work until
after readiness.

After each process closes, the runner checks every document hash against the
fixture and compares normalized semantic store contents across configurations.
An unchanged restart must preserve the store epoch and write generation, and
the profiling build must report no recovery, rebuild or document derivation. Raw
samples retain the full phase hierarchy and logical work counters. Results are
diagnostic observations; elapsed-time thresholds do not enter PR gates.
After a verified fresh/restart pair, its disposable vault and derived state are
removed. Each run retains one frozen corpus, all executables, runner sources,
audit evidence and logs, so accumulated work trees do not bias later cells.

Run the focused harness checks:

```sh
python3 -m unittest discover -s scripts/performance -v
cargo test --locked -p norn-host --test performance --features induced-failure --no-run
```

The second command compiles the explicit measurement target. It has
`harness = false` and `test = false`, so ordinary suite execution does not run
it and it claims no ignored CI test lane. The baseline runner invokes its
`generate` and `serve` modes with issued roots and tokens. The profiling build
also exposes `audit`, which reads the closed store through its owning crate.
PR and full-regression workflows explicitly lint the profiling target and run the Python
checks without executing a benchmark or requiring the legacy binary.
Its functional trial can use one scale, one warmup, two samples, and one
repetition before collecting a full baseline. Measurement records require an
independent validity review and a separate reproduction.

## Record a timing run

The record mode measures one engine and writes one flat record of metric rows,
so a scheduled job can keep a timing history of current main and notice a
regression before a release. It measures only the current engine, or only the
legacy engine, and no comparison runs inside it.

Build the profiling worker, which carries the public first-open mode and the
startup work counters, then record:

```sh
python3 scripts/performance/provenance.py \
  --startup-profile \
  --target-directory "$PERF_HOME/build" \
  --receipt "$PERF_HOME/record-build.json"
python3 scripts/performance/record.py \
  --current-build "$PERF_HOME/record-build.json" \
  --fixtures "$PERF_HOME/record-fixtures" \
  --output "$PERF_HOME/record-001"
```

The first run generates each missing fixture size in `--fixtures` with the same
generator and probes as the baseline, moving it into place only once complete. Later runs check the frozen digest and reuse
the bytes unchanged. The defaults are 2,000 and 10,000 background documents
(2,014 and 10,014 with probes) and three fresh processes per size. Each warm
operation runs five warmups and 400 measured calls. Writes also exclude their
first 200 calls, which carry the one-time page-split cost after a from-zero build.
`--processes`, `--samples`, `--warmups` and `--write-payoff` change these numbers.
The timing recorder stays off; the feature build is a measurement subject, not a
shipping-binary claim.

Each current process attaches a fresh copy of the fixture and issues the public
first-open Get and Find as attachment begins. The parent releases each first read
when its answer arrives and validates both against the authored fixture after
Ready, so validation never delays the second read. The worker's clocks start when
it adopts the vault:

| Metric | Meaning |
| --- | --- |
| `first_get`, `first_find` | Worker clock when the first whole Get and the first metadata Find return a served answer. A refused first read is `unavailable` and has no value. |
| `ready` | Worker clock when the caller first observes Ready. |
| `warm.<operation>.median`, `warm.<operation>.p95` | Round-trip latency of the twelve baseline workloads, each answer verified as in the baseline. |
| `work.<counter>` | Startup work: documents derived, document opens, changesets applied, links judged, findings written and discarded. |
| `work.bytes_written` | Bytes the worker passed to write calls through startup, less its own measurement output. |

Clocks take the median across processes. A warm operation reports the median of
its per-process medians and the median of its per-process p95 values. A counter
must be equal in every process; a disagreement is recorded as `unstable` with each
value in the note. Watcher polls and checkpoint retries are not recorded, because
they follow timing rather than work.

`--engine legacy --legacy "$LEGACY_NORN"` records the installed legacy daemon over
fixtures a current run already generated: `ready` is its hello-to-ready time,
`first_query` is spawn to the first validated common-text search, and the warm
operations match. Legacy reports no first reads and no counters.

`record.json` holds the run identity (commit, executable hash, compiler, start
time and load), the parameters and the rows. A failed run keeps its failure and
no rows. Worker logs remain under `work/`; vaults and stores are removed after
each process.

### Keep a timing table

`timeline.py` keeps a tab-separated table with one row per metric and run:

```sh
python3 scripts/performance/timeline.py append "$TABLE" "$PERF_HOME/record-001/record.json"
python3 scripts/performance/timeline.py skip "$TABLE" --commit "$COMMIT" --reason "another build is running"
python3 scripts/performance/timeline.py compare "$TABLE"
```

The columns are `run_at`, `commit`, `build_sha256`, `compiler`, `documents`,
`metric`, `value`, `unit`, `status`, `load_start` and `note`. Only a completed
record is appended. A skipped run is one `run` row with status `skipped` and its
reason, so a gap stays visible. Keep one table per engine.

`compare` reports on the latest run. A skipped latest run is reported alone; the
measured run before it was reported on its own night. Otherwise it reports:

- any metric that is `unavailable` or `unstable`;
- a work change: any counter that differs from its last measurement in an earlier
  run, so an unstable night in between does not hide a change. Between
  two commits, or two executables of one dirty commit, it is a change in work; for
  the same commit and executable it is reported as nondeterministic;
- a sustained clock shift: the latest three measured values are each at least 1.2
  times, and at least the floor above, the median of up to seven measured runs
  before them. The floor is 100 ms for `first_get`, `first_find`, `ready` and
  `first_query` and 0.5 ms for warm operations. At least three reference runs are
  required.

`--ratio`, `--cold-floor-ms`, `--warm-floor-ms`, `--sustain` and `--window` change
the thresholds. A shift stays reported until it enters the reference window, and
only while the latest run measures that clock. Skipped runs are ignored by both
comparisons. Counters assume the frozen fixtures never change: a regenerated
fixture moves every counter and reads as a work change.
