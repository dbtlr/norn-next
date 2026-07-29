# Decisions

The index of decision records for `norn`, one line per ADR.

## Index

| ADR | Decision |
|---|---|
| [0001](0001-corpus-activation-gate.md) | The coverage corpus is evidence with zero authority; activation is a per-command approval act judging goodness, every command sits in exactly one of three disjoint categories, and a case is judgeable only against its recorded input tree. *Amended 2026-07-28: a case materializes that tree from its own manifest, which carries every entry's exact bytes, so activation never depends on a generator reproducing it.* |
| [0002](0002-fixture-determinism-and-calibration.md) | A generated fixture tree is a function of `(profile, seed)` alone, proven by a tree digest before anything depends on it; realism is five seeded knobs led by a long-tailed body length; and calibration is an authored, checked-in envelope that moves only by a reviewed edit. *Amended 2026-07-28: the soak lane is built and runs the ≥5k cases nightly, and the leaf claim is a normal-dependency claim — one dev edge on norn-testkit carries the memory bars.* |
| [0003](0003-boundary-enforcement-harness.md) | The dependency allowlist is gated by equality restricted to the crates the workspace has earned; symbol-level rules arrive one at a time in one workspace-root ruleset, carved out at the use site; and the mapping from each boundary invariant to what carries it is executable harness data checked against both. |
| [0004](0004-two-tier-measurement-and-authored-baselines.md) | Counts gate per PR — a peak-memory ceiling and a flatness pair over two sub-5k scales — and clocks only trend in soak, where a wall-clock bar is a sanity ceiling rather than a gate; a trend's whole memory is a checked-in authored baseline moved only by a reviewed edit, with the direction it moves review-held; a lane step asserts that cases ran; and a bar's subject is whatever exists, with the slot naming the layer that will hold it. |
| [0005](0005-regression-stratum.md) | The regression stratum is a registry of named cases, each a falsifiable present-tense property checked by counters and structure rather than by comparing bytes; every case names the venue — the first layer that can bind it — and dormancy is structural, ended by an edit naming the tests that carry it, with a dormant case at the layer that already exists owing a stated reason; five ratified classes and the case total are pinned in code. |
