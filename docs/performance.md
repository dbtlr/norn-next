# Measure the runtime baseline

The baseline runs twelve search, get, and write workloads through the current
production host and an isolated legacy daemon. It preserves raw observations,
known-answer fixtures, executable identity, and a self-contained HTML report.

These measurements describe the selected workflows. They are not acceptance
thresholds, correctness oracles, CLI measurements, or long-duration leak tests.
The current composition root has no serving surface yet. The current side uses
an opt-in integration-test worker, which adds no product dependency or API.

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
The worker polls readiness every millisecond instead of using the test helper's
adaptive backoff, whose maximum interval is 50 milliseconds.

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

## Simulate layered loading

`loading.py` compares ordinary loading with an explicitly enabled experiment on
disposable fixture copies. The experiment inventories names into bounded,
disk-backed storage, loads the requested document, writes frontmatter and bodies,
adds rich Markdown facts, and finally judges link health over the complete facts.
The ordinary full-text triggers run with the body writes, so the first experiment
has one frontmatter-plus-lexical checkpoint.

The checkpoint observer runs real store query builders on store-minted readers.
It records the first slug get, metadata predicates, lexical queries, a section
get, and a validation summary. Coverage counts check that the metadata and lexical
checkpoints include the complete fixture. The final normalized store must match
an ordinary build. Catalogue entries describe observed names; they do not certify
content eligibility before a candidate is read.

```sh
python3 scripts/performance/loading.py \
  --build "$PERF_HOME/startup-build.json" \
  --fixtures "$PERF_HOME/run-001/fixtures" \
  --output "$PERF_HOME/loading-001" \
  --sizes 300 2000 10000 --repetitions 2
```

This is an isolated loading-cost experiment, not a production admission policy.
The normal host remains warming until all stages and watcher reconciliation finish.
Checkpoint probes currently pause the loading worker while they read. Their costs
remain inside total completion time, and ordinary warm requests use the normal host
after completion. Continuous request traffic during loading needs its own measurement.
The `layered-silent` control records boundaries without running intermediate queries.
Worker-clock checkpoint boundaries and loading spans are distinct from request,
serialization, controller-validation and resource-sampling clocks. Frontmatter and
lexical queries share a boundary in this first experiment. The frontmatter checkpoint
admits field predicates, not the tag facet, which also depends on body tags.

Failed experimental stores retain an incomplete marker. Every ordinary store open
refuses that marker, including builds without experimental support. Completed imports
remove it and restore the writer's original temporary-storage settings. No crash-resume
or live-vault migration claim is made. Full-text rank-1 and store integrity checks run
after shutdown, outside startup clocks.

The experiment is compiled only with `induced-failure` and activated by the
measurement worker's explicit loading selection. It does not change ordinary
changeset semantics or an accepted architectural decision. It intentionally observes
partial projections and defers health outside the ordinary per-changeset contract.
Semantic model loading and inference are not measured: this checkout has only the
deterministic stub embedder, and the measurement worker composes no semantic engine.
