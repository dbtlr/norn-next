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
identity. The three immutable worker copies have separate receipts.

```bash
mkdir -p target/scratch
export TMPDIR="$PWD/target/scratch"
python3 scripts/performance/loading_matrix.py --prepare-builds "$PWD/target/loading-builds"
python3 scripts/performance/loading_matrix.py --prepare-plan --sizes 2014 10014 > target/loading-plan.json
python3 scripts/performance/loading_matrix.py --config target/loading-builds/config.json --sizes 314 --output target/loading-pilot
```

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
to parent clocks. Per-operation `host_ns` and `round_trip_ns` keep their existing
worker and controller boundaries. These are descriptive observations from one
sample, with no statistical or speedup claim.

- `ready`: four-step structure-complete callback; main's public Ready control
  record; health-once's full-build health boundary. `public_ready` separately
  retains the ordinary control record for every arm. These parent observations
  include delivery delay and, for the public control record, the worker's
  accounting tail after observing Ready.
- `health_complete`: four-step relations-complete callback; public Ready for main;
  full-build health boundary for health-once.
- `first_get` and `first_find`: the first validated answer. Main sends one real
  public `Host::get` and `Host::find` during ordinary first attachment, using its
  existing caller-only ACK protocol; an ordinary loading refusal falls back
  after Ready. Four-step uses `FourStepRead::get/find`, explicitly labelled as lab
  hooks. Health-once uses public calls after its full build. All arms issue the
  same logical whole Get and active metadata first page; the lab's Find projects
  only paths, as the public query does. Lab answers are checked against final
  public answers. Hook latency and public admission latency are different paths.
- `requested_document_structure`: the lab's requested-document structure instant,
  descriptive only. Whole Get still waits for content to finish vault-wide.
- `first_search`: an actual lexical `Host::search` for the existing common token,
  independently checked against the fixture. It runs after public Ready in every
  arm. **The step-2 search answer time is unavailable:** the lab read hook has no
  search method and rejects text Find. `content_complete` is a boundary, not a
  claimed search response.
- `step_counters`: the prototype report's per-step rows, facts, link judgments,
  findings, commits, and file reads. `step_boundary_intervals_ns` subtracts the
  worker's successive callback clocks. These intervals include observer work;
  the names interval includes setup, and the content interval includes the
  requested document's pull-ahead structure. Pure step durations are unavailable.
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
- `documents_layout`: read-only SQLite `dbstat`, documents B-tree only. Page count
  includes interior, leaf, and overflow pages. Average fill is total payload bytes
  divided by total page bytes. Fragmentation is the fraction of consecutive pages
  in `dbstat.path` order whose physical page numbers are not adjacent. This is a
  defined physical-order measure, not an estimate of query cost.
- `ready.resources` and `end.resources`: Linux main-process RSS and related
  readings when the controller observes the boundary. Samples exclude the
  controller; they do not claim the exact publication instant.
- `replay_verified`: the existing native provenance validator checks the complete
  declared short workload against final epoch/generation and plan-root identity.
  Retained responses must exactly equal the post-Ready raw worker stream.

Health-once cells start a separate Python adapter process with the archived
checkout's `scripts/performance` first on its import path. Its own `transport` and
`loading.validate_receipts` select and validate `health-once`; they are not ported.
The controller uses the same current shared provenance replay validator and layout
inspection for the four arms. The archived final audit uses its own audit adapter.

Run the Python checks with the repository's standard runner:

```bash
TMPDIR="$PWD/target/scratch" python3 -m unittest discover -s scripts/performance -v
TMPDIR="$PWD/target/scratch" python3 -m unittest discover -s scripts/performance/live_mode -v
```
