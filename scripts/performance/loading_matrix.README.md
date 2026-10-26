# Run the loading lab comparison

The controller runs one fresh measured worker per cell, serially. It uses release
builds with `induced-failure`, the repository toolchain, 128-document changesets,
and 128-document four-step transactions. It never restarts a measured cell.
Fixture generation and the post-close audit run in separate, untimed processes.
The operating system's filesystem caches are uncontrolled.

Run these commands from the harness checkout after committing its worker changes.
All generated files and detached source checkouts stay under its ignored `target`.
The build step takes exact main and health-once revisions and the harness's
current committed prototype revision. Each revision's `provenance.py` runs
`rustup run` and retains compiler, Cargo, source, profile, feature, and binary
identity. Each revision has its own Cargo target directory, immutable worker copy, and receipt.
Separate targets prevent reuse of incompatible local-crate artifacts across revisions.
At run time the prototype Rust sources must match this checkout; Python-only
follow-up changes do not require recompilation.

```bash
mkdir -p target/scratch
export TMPDIR="$PWD/target/scratch"
python3 scripts/performance/loading_matrix.py --prepare-builds "$PWD/target/loading-builds"
python3 scripts/performance/loading_matrix.py --prepare-plan --sizes 2014 10014 > target/loading-plan.json
```

After preparation, run the 314-document pilot explicitly:

```bash
python3 scripts/performance/loading_matrix.py --config target/loading-builds/config.json --sizes 314 --output target/loading-pilot
```

The pilot needs `target/loading-builds/config.json` and the source checkouts,
build receipts, and immutable worker binaries that configuration names.
Preparation creates these files. A changed Rust worker requires a new committed
prototype revision and a fresh build directory. Reuse no existing output directory.

The pilot runs each arm once. To select an arm, add exactly one of
`--arms main`, `--arms four-step`, `--arms four-step-no-body`, or
`--arms health-once` and use a new output directory. For an existing frozen fixture
parent, add `--fixtures PATH`; its subdirectories use background counts `300`,
`2000`, and `10000`. Otherwise `baseline.prepare` generates and freezes the fixture
once per size using the existing seed and known-answer probes. Total document
counts include fourteen probe documents. `small` resolves to `bench/small.md` in
all arms; the lab receives that exact path as its requested document.

The larger matrix is an explicit invocation, separate from preparation:

```bash
python3 scripts/performance/loading_matrix.py --config target/loading-builds/config.json --sizes 2014 10014 --output target/loading-matrix
```

Each cell writes `result.json`, `cell.json`, its fixture manifest, raw worker and
audit logs, and the complete request/response history. The run's `run.json` stays
incomplete on any failed cell or semantic mismatch. It sets `semantic_equal`
only when all four arms ran and passed the gate for each identical fixture.

## Read the measurements

All `parent_received_ns` values share one origin per cell: immediately before
constructing the existing `Current` adapter. They include process launch,
serialization, transport, and earlier validation or sampling. Worker clocks have
the worker's existing `Instant` origin and remain separate; they are never added
to parent clocks. Report `step_boundary_ns` values have a third origin: the lab
build's start, after host setup. Per-operation `host_ns` and `round_trip_ns` keep their existing
worker and controller boundaries. These are descriptive observations from one
sample, with no statistical or speedup claim.

Lab callbacks buffer typed answers and timing records. The worker serializes and
emits them after construction and accounting finish, before its public Ready record.
Their `parent_received_ns` values describe delivery after build, not early availability.
Console output separates the estimated earliest answer, actual query finish,
and parent delivery times.

- `ready`: four-step structure-complete callback; main's public Ready control
  record; health-once's full-build health boundary. `public_ready` separately
  retains the ordinary control record for every arm. Lab parent observations are
  buffered delivery after build. The public control record includes the worker's
  accounting tail after observing Ready. Console output names the selected
  boundary and prints public Ready beside it.
- `health_complete`: four-step relations-complete callback; public Ready for main;
  full-build health boundary for health-once.
- `first_get` and `first_find`: the first validated answer. Main sends one real
  public `Host::get` and `Host::find` during ordinary first attachment, using its
  existing caller-only ACK protocol; an ordinary loading refusal falls back
  after Ready. Four-step uses `FourStepRead::get/find`, explicitly labelled as lab
  hooks. Health-once uses public calls after its full build. All arms issue the
  same logical whole Get and active metadata first page; the lab's Find projects
  only paths, as the public query does. Lab answers are checked against final
  public answers. The lab Get is labelled **answered before link health (health loading)**.
  Main and health-once Get answers include link-health findings. The base fixture's
  small document has no links, so equal answers do not make these findings guarantees equal.
  Lab timing is labelled **estimated earliest availability plus builder query time**.
  `earliest_available_ns` means prerequisite completion observed on the worker clock,
  rather than the exact transaction completion instant. For Get, it is the later
  of ContentComplete callback entry and that document's Structured callback entry.
  Find uses ContentComplete entry. Search uses ContentComplete entry in standard
  mode and StructureComplete entry in no-body mode.
  `earliest_answer_ns` adds the measured `query_host_ns` to that prerequisite observation.
  Lab endpoint `worker_ns` uses this derived estimate, which excludes dispatch
  delays caused by preceding probes. It is not an actual answer-finish observation.
  The estimate reuses the measured builder duration; no earlier-dispatch execution is measured.
  Actual `query_started_ns` and `query_finished_ns` remain separate worker-clock observations.
  Hook latency and public admission latency are different paths.
- `requested_document_structure`: the lab's requested-document structure instant,
  descriptive only. Whole Get still waits for content to finish vault-wide.
- `first_search`: the first actual lexical answer for the existing common token,
  independently checked against the fixture. Four-step calls `FourStepRead::search`
  after content completes. No-body calls it after structure completes, when all
  body text is present. Main and health-once use their first public `Host::search`
  after Ready. Every result names the path used. `public_search_after_ready` retains
  a public search in every arm, including the lab arms.
- `step_counters`: the prototype report's per-step rows, facts, link judgments,
  findings, commits, and file reads. `files_read` counts source-read attempts, including failed opens and paths refused before opening.
  `rows_rewritten` counts content/metadata transitions. `body_rows_rewritten` counts
  the separate body payload updates in no-body mode. SQLite's internal FTS trigger
  and shadow-table writes are uncounted. Skipping findings discard on a fresh store
  is also available to ordinary loading; that saving is not specific to four-step.
  `step_boundary_ns` records names, content, structure, and relations end times
  relative to the lab build's start. `step_boundary_intervals_ns` subtracts those
  successive boundaries; intervening observer queries and pull-ahead work remain included.
  `step_busy_ns` records each main pass's elapsed work, excluding observer callbacks
  and requested-path pull-ahead. `pull_ahead_ns` records that requested-path work
  separately, excluding callbacks. Busy time is elapsed work, not CPU time.
  `worker_ready.job_evidence` retains ordinary counters, including links decided,
  findings written/discarded, and changesets. Ordinary commit counts are
  unavailable; changesets are not substituted for commits.
- `warm_operations`: the existing eight reads and four writes, one sample each,
  in their existing order. Each operation retains its command, response, host
  time, round trip, counters, and validation. Write readbacks are separate history
  entries outside the operation's clock. There is no aggregate warm score.
- `store`: the existing store-owned schema-3 audit after all warm writes and
  worker shutdown. Source paths, byte lengths, and hashes are checked against
  the actual final tree. `semantic_sha256` must match across arms sharing the
  original frozen fixture. Canonical identity, page map, and document construction
  hashes remain descriptive; physical differences do not fail the semantic gate.
- `documents_layout`: read-only SQLite `dbstat` after the four warm writes, worker
  shutdown, and final audit. It describes the post-workload layout; it does not
  isolate the layout produced by loading alone. It covers the documents B-tree only. Page count
  includes interior, leaf, and overflow pages. Average fill is total payload bytes
  divided by total page bytes. Fragmentation is the fraction of consecutive pages
  in `dbstat.path` order whose physical page numbers are not adjacent. This is a
  defined physical-order measure, not an estimate of query cost.
- `ready.resources` and `end.resources`: Linux main-process RSS and related
  readings when the controller receives the record. Lab boundary records arrive
  after build, so their resources are not early-boundary samples. Samples exclude
  the controller; they do not claim the exact publication instant.
- `replay_verified`: the existing native provenance validator checks the complete
  declared short workload against final epoch/generation and plan-root identity.
  Retained responses must exactly equal the post-Ready raw worker stream.

Health-once cells start a separate Python adapter process with the archived
checkout's `scripts/performance` first on its import path. Its own `transport` and
`loading.validate_receipts` select and validate `health-once`; they are not ported.
The controller uses the same current shared provenance replay validator and layout
inspection for the four arms. The archived final audit uses its own audit adapter. Its exactly-once receipt gate
uses the final audited link count: the fixed, validated warm workload changes no
link cardinality, so this is also the initial stored link count.

Run the Python checks with the repository's standard runner:

```bash
TMPDIR="$PWD/target/scratch" python3 -m unittest discover -s scripts/performance -v
TMPDIR="$PWD/target/scratch" python3 -m unittest discover -s scripts/performance/live_mode -v
TMPDIR="$PWD/target/scratch" CARGO_TARGET_DIR="$PWD/target" cargo test -p norn-host --test performance_timing --features induced-failure
```
