# Decisions

The index of decision records for `norn`, one line per ADR.

## Index

| ADR | Decision |
|---|---|
| [0001](0001-corpus-activation-gate.md) | The coverage corpus is evidence with zero authority; activation is a per-command approval act judging goodness, every command sits in exactly one of three disjoint categories, and a case is judgeable only against its recorded input tree. *Amended 2026-07-28: a case materializes that tree from its own manifest, which carries every entry's exact bytes, so activation never depends on a generator reproducing it.* |
| [0002](0002-fixture-determinism-and-calibration.md) | A generated fixture tree is a function of `(profile, seed)` alone, proven by a tree digest before anything depends on it; realism is five seeded knobs led by a long-tailed body length; and calibration is an authored, checked-in envelope that moves only by a reviewed edit. |
| [0003](0003-boundary-enforcement-harness.md) | The dependency allowlist is gated by equality restricted to the crates the workspace has earned; symbol-level rules arrive one at a time in one workspace-root ruleset, carved out at the use site; and the mapping from each boundary invariant to what carries it is executable harness data checked against both. |
