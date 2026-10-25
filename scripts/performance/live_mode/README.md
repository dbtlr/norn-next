# Public and private first-open measurement adapter

This adapter selects `Experiment::live_metadata("small", observer)` in the existing
performance worker. It measures two private requests on the attachment thread:
whole-record slug Get, then complete metadata Find. Catalogue and final-health
callbacks issue no queries. The worker remains feature-private; ordinary verbs,
read admission, frozen modes, writer settings, and semantic scheduling retain their
existing behavior.

Each live request retains its acquisition reading, full snapshot counters, and
actual builder-invocation flag. Successful builders retain their typed snapshot
and work before conversion to a report. A builder whose answer loses eligibility
retains that answer as withdrawn evidence and supplies no logical availability.
Construction declines explicitly identify Get or Find. Construction eligibility
Get receipts are separate from logical P2 and come from the existing invocation.
Errors fail the cell.

Main runs ordinary P2 after Ready. Candidate attempts private P2, then runs ordinary
P2 after Ready for revalidation or to supply unavailable logical answers. Both run
the existing 481-call suffix: common search, 60 warmups, 300 measured operations,
and 120 confirming Gets. Both have 483 canonical calls; a successful candidate has
485 physical requests, versus main's 483. Construction eligibility work is reported
separately. No extra fallback query is issued.

The physical account's `total_builder_dispatches` counts the 483 canonical calls
plus private attempts whose builder was invoked. `builder_not_invoked_attempts`
counts private attempts without a builder invocation. These are not SQL counts:
snapshot acquisition can execute statements before a guard refuses the builder.
Actual SQL work remains in the snapshot counters. These private output names apply
to new records; historical measurement records retain their original schema.

`controller.py` takes `--config` and `--output`. The configuration supplies source
paths and hashes, worker paths and build receipts, compiler hash, fixture proofs,
and tool-file hashes. `performance_tools` points to this repository's performance
scripts, `existing_bundle` to capped-prefix-execution-r3, and `prototype_tools` to
canonical-revalidation-prototype-r2/tools. No frozen bundle is modified. The owned
native validator and capture module derive from that prototype; live envelope
handling preserves its content, source, provenance, counter, and index checks.
The content oracle shared with ordinary Get accepts a separately validated live
envelope without manufacturing Ready trust or paused/frozen metadata.

The default invocation checks inputs and starts no workers. `--execute-functional`
executes exactly one fresh 314-document cell; `--arm` chooses `main-ordinary` or
`candidate-live-revalidated`. `--compare MAIN_OUTPUT CANDIDATE_OUTPUT` compares
both completed cells, including canonical P2 counters and the complete suffix.
`--prepare-plan` writes the fixed eight-run plan and starts no subjects. `--size` and `--execute-cell` provide one planned cell per invocation.
Large-cell execution remains parent-owned and requires subsequent review.

All endpoints use one parent origin before transport. Worker clocks stay separate.
Warm reads use default round-trip latency; mutations retain write plus confirming
readback latency. Every operation keeps five warmups, 25 samples, host diagnostics,
and the existing resource snapshots. Resource endpoints do not establish true
peaks or final byte I/O. Functional clocks are nonqualifying. The logical index
digest does not establish embedding quality; no semantic engine is composed here.

The planned comparison is descriptive: exact results and cold gains are mandatory,
and warm differences require judgment without a 1% hard gate. It has two pairs per
size, no independent-call assumption, confidence intervals, A/A subtraction, or
statistical pass. Every outcome is retained; the first failed or interrupted cell
stops execution pending parent disposition. Completed cells are never repeated.
Owned heavy work closes before future sampling and processes run serially. OS
caches remain uncontrolled and ambient T3 Git polling prevents a whole-host quiet
claim.


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
Environment fingerprints retain no secret values. Historical records keep their
original schema.

Public functional clocks remain permanently nonqualifying. Larger comparisons are
descriptive: warm differences are judged per operation, with no hard percentage,
equivalence, confidence, or statistical gate. Resource endpoints cannot establish
continuous peaks or final byte I/O. There is no timed in-process sampler or
full-tree observer. Ordinary baseline tombstone work remains unavailable.
