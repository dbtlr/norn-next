# Private live measurement adapter

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
`--prepare-plan` writes the fixed eight-run plan and starts no subjects. Large-cell
execution remains parent-owned and requires subsequent review.

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
