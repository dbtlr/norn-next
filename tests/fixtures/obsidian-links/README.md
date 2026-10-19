# Observe Obsidian link resolution

This probe collects Obsidian observations for NORN-422.
[ADR 0040](../../../docs/decisions/0040-a-link-resolves-as-obsidian-resolves-it.md) records the resolution rule, and the parity suite named below holds norn to it.

[Obsidian 1.14.5 on iPhone](observations/ios-1.14.5/README.md) records the first opening pass and one captured rename.
Those observations include changed inputs and an opening error; they do not certify the original tree or other platforms.

## Install the probe

Install the probe into an existing Obsidian vault from the repository root:

```sh
cargo run -p norn-testkit --bin norn-obsidian-probe -- <vault-root>
```

The command copies `vault/` into `<vault-root>`, preserving every relative path, and renames the files [install.json](install.json) lists under `renamed_on_install`.
A plain copy of `vault/` is not an install, because it leaves those files composed.
The destination must be fresh: the command refuses when `artifacts/scratch/norn-422` already exists.
Keep the vault root unchanged. Do not open the scratch folder as a separate vault.
No Obsidian settings, plugins, or existing documents need installation or replacement.

Open [the starting guide](vault/artifacts/scratch/norn-422/n422-start.md) in the destination vault.
The guide and results sheet stay inside the synced folder.
Copy `n422-expectations.md` back beside this README after observations, preserving the original fixture tree.
Keep changed source documents as supporting evidence outside `vault/`.

## Interpret the evidence

The exact input tree lives in `vault/`. [cases.json](cases.json) records case IDs, source paths, literal links, and intended competitors.
Candidate lists describe the setup, not the expected destination or an exhaustive inventory of the receiving vault.
Some candidates deliberately have similar names that a resolver must distinguish.
Numbered link aliases identify cases without changing their destinations.

The fixture uses the fixed vault-relative prefix `artifacts/scratch/norn-422`.
That prefix is an input to rooted and exact-path probes. Moving the folder changes those probes.
Full-path cases distinguish a vault path from a deeper suffix with the same trailing path.
Source-local competitors distinguish relative resolution from vault-wide matching.

`cases.json` spells every path at its intended code points, and two of them are decomposed (NFD) beside composed twins.
Git does not keep a decomposed name portably, so those two files are checked in composed and the install renames them.
`cases.json` is the baseline the recorded observations pin by hash, so the rename list lives in `install.json` instead.
Filesystem and sync normalization can still change the names actually presented to Obsidian.
Results without actual filename evidence do not settle NFC versus NFD behavior.
A case-insensitive device does not establish case folding on a case-sensitive filesystem.
Record these limitations and repeat affected cases on a suitable filesystem before calling them confirmed.

The observations are manual external-application observations of Obsidian, not assertions about Norn.
The iOS recording provides exploratory evidence for the resolution ADR.
Cross-platform behavior and unobserved link-writing combinations remain outside its verified coverage.

`crates/norn-host/tests/obsidian_parity.rs` asserts norn's answer for each recorded case against an authored expectation, and checks that expectation for compatibility with these observations.
