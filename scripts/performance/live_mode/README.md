# Public first-open measurement adapter

`controller.py` takes `--config` and `--output`. The configuration supplies source
paths and hashes, worker paths and build receipts, compiler hash, fixture proofs,
and tool-file hashes. `performance_tools` points to this repository's performance
scripts. `existing_bundle` and `prototype_tools` point to the pinned external
runner and shared audit oracles used by the public comparison.

The default invocation checks inputs and starts no workers. `--execute-functional`
executes one fresh 314-document cell. `--arm` chooses `main-ordinary` or
`candidate-public-revalidated`. `--compare MAIN_OUTPUT CANDIDATE_OUTPUT` compares
completed cells, including canonical P2 counters and the complete suffix.
`--prepare-plan` writes the fixed eight-run plan and starts no subjects.
`--size` and `--execute-cell` select one planned cell per invocation.

## Public Host mode

`candidate-public-revalidated` selects `operator-prefix-public` in the existing
worker. It composes `Vault::host_with_policy` and calls `Host::get` for whole
`small`, then `Host::find` for the same complete metadata page as canonical P2.
Targets enter through those calls. Construction receives no target or observer.
Parent ACKs wait on the caller thread while attachment continues normally.

Public events retain the genuine `VaultAnswer`, its actual trust, epoch and
generation, snapshot counters, and `Answered.work`. Caller start and finish clocks
exclude response serialization. A separate post-call state observation records
when the caller first observes Ready. `early_answer` requires a warming answer
and a post-call observation that is still warming. A Ready answer is served
publicly and earns no early label. A warming hold that finishes after promotion
also earns no early label when its post-call observation sees Ready. Publication
itself has no timestamp in this API. Parent validation endpoints and worker
observations remain separate clocks.

A loading or withdrawal refusal retains its actual error envelope and elapsed
cost. It supplies no answer, snapshot counters, or builder work. Other errors
stop the cell. The single canonical P2 supplies every unavailable logical answer;
there is no additional fallback request or renewed acquisition deadline.
Every arm has 483 canonical calls, including the unchanged 481-call suffix.
The public candidate adds exactly two public attempts regardless of disposition.
Refusals expose neither builder execution counts nor before-builder counters.
Those accounts remain unavailable. Public construction has no private eligibility
Get receipt.

`job_evidence` reports actual total startup work, including additional public
construction. Its canonical and private components are unavailable. The pair
compares exact canonical P2 and suffix answers and snapshot counters, source
histories, and four final identities. Startup writer totals are reported for
judgment, without asserting an unobservable split or equality. `semantic_sha256`
is a logical-store identity and says nothing about semantic quality.

Set `candidate_mode` to `candidate-public-revalidated` in the pinned configuration
for the public eight-cell plan. `--replay CELL ...` rechecks retained raw producer
streams, envelopes, counters, source mutations, and final audits without launching
workers. The output path must be fresh. New producer receipts bind actual cwd and
an exact environment fingerprint alongside the existing selected environment
values. `execution-before.json` pins the controller interpreter, cwd, argv,
environment fingerprint, and source and tool inputs before launching subjects.
The complete fingerprint identifies the original launch environment. Replay
checks the selected operational values and retains that original fingerprint; it
does not substitute its own ambient environment, whose harness call ID can differ.
Unselected environment values are not retained or reconstructed. Environment
fingerprints retain no secret values. Historical records keep their original schema.

Public functional clocks remain permanently nonqualifying. Larger comparisons are
descriptive: warm differences are judged per operation, with no hard percentage,
equivalence, confidence, or statistical gate. Resource endpoints cannot establish
continuous peaks or final byte I/O. There is no timed in-process sampler or
full-tree observer. Ordinary baseline tombstone work remains unavailable.
