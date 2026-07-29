# Decisions

The durable decisions that govern `norn`. Status and date describe the decision itself, not the implementation task or the date this index was edited.

| ADR | Status | Date | Decision |
|---|---|---|---|
| [0001](0001-corpus-activation-gate.md) | Accepted | 2026-07-27 | The coverage corpus is evidence with zero authority; explicit approval activates a command only after judging its behavior independently. |
| [0002](0002-fixture-determinism-and-calibration.md) | Accepted | 2026-07-28 | Fixture identity is build-scoped and covers the paths and bytes emitted by the generator, not filesystem-normalized readback. |
| [0003](0003-boundary-enforcement-harness.md) | Accepted | 2026-07-28 | The authoritative invariant-to-enforcement mapping is executable and checked in both directions. |
| [0004](0004-two-tier-measurement-and-authored-baselines.md) | Accepted | 2026-07-27 | CI lanes divide by evidence kind: stable structure may gate pull requests; clocks and trends belong to soak. |
| [0005](0005-regression-stratum.md) | Accepted | 2026-07-28 | Unbound regression obligations live as one case per property in a registry that names each case's earliest binding venue. |
| [0006](0006-recorded-inputs-are-self-contained.md) | Accepted | 2026-07-28 | Recorded corpus inputs carry their own exact bytes and never depend on fixture regeneration. |
| [0007](0007-authored-measurement-thresholds.md) | Accepted | 2026-07-28 | Measurement thresholds are authored constraints changed by reviewed edits, never self-derived from observations. |
| [0008](0008-present-crate-dependency-equality.md) | Accepted | 2026-07-28 | Exact dependency equality applies to normal edges between crates currently present in the earned workspace. |
| [0009](0009-regression-properties-not-recordings.md) | Accepted | 2026-07-27 | Regression behavior is verified with present-tense properties, structural assertions, and counters rather than recorded-byte equality. |
