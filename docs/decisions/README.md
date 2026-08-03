# Decisions

The durable decisions that govern `norn`. Status and date describe the decision itself, not the implementation task or the date this index was edited.

| ADR | Title | Status | Date | Rationale |
|---|---|---|---|---|
| [0001](0001-corpus-activation-gate.md) | The coverage corpus has zero authority | Accepted | 2026-07-27 | Treating historical recordings as expected tests would silently turn legacy mistakes into requirements. |
| [0002](0002-fixture-determinism-and-calibration.md) | Fixture identity is build-scoped | Accepted | 2026-07-28 | Generator revisions may intentionally move a tree, while filesystem normalization is outside generator behavior. |
| [0003](0003-boundary-enforcement-harness.md) | Invariant-enforcement authority is executable | Accepted | 2026-07-28 | A prose enforcement ledger can drift from the mechanisms that actually carry each invariant. |
| [0004](0004-two-tier-measurement-and-authored-baselines.md) | CI lanes divide by evidence kind | Accepted | 2026-07-27 | Evidence keeps the same owner when machine speed or observed runtime changes. |
| [0005](0005-regression-stratum.md) | Unbound regression obligations live in a registry | Accepted | 2026-07-28 | Obligations must survive before their subjects exist without pretending they are already tested. |
| [0006](0006-recorded-inputs-are-self-contained.md) | Recorded corpus inputs are self-contained | Accepted | 2026-07-28 | Recorded evidence must remain judgeable as the independent fixture generator evolves. |
| [0007](0007-authored-measurement-thresholds.md) | Measurement thresholds are authored constraints | Accepted | 2026-07-28 | A measurement-derived threshold would ratify drift and depend on history that may disappear. |
| [0008](0008-present-crate-dependency-equality.md) | Dependency equality is restricted to present crates | Accepted | 2026-07-28 | The target crate map must stay ahead of implementation without requiring unearned edges or permitting stale ones. |
| [0009](0009-regression-properties-not-recordings.md) | Regression behavior is expressed as properties | Accepted | 2026-07-27 | Recorded-byte equality cannot reveal behavioral shape, scope, or cost. |
| [0010](0010-frontmatter-value-model.md) | The frontmatter value model is derived from its consumers | Accepted | 2026-07-29 | Fidelity no consumer can hold would relocate coercion from one parse boundary to every consumer. |
| [0011](0011-shadows-live-outside-the-vault.md) | Shadows live outside the vault | Accepted | 2026-07-31 | Mechanism files placed among documents get committed and synced by vault-side tools; rename's same-filesystem law shapes the per-vault fallback. |
| [0012](0012-maintainer-singleton.md) | The maintainer is singular, the vault is not | Accepted | 2026-07-31 | Exclusion cannot carry correctness in a multi-writer vault; only the derived-state maintainer must be singular. |
| [0013](0013-quarantine-is-per-document.md) | Quarantine is per document, refusal is per vault | Accepted | 2026-08-03 | One unreadable document must not withdraw a whole vault, and widening the same treatment to a broken environment would turn it into silent data loss. |
