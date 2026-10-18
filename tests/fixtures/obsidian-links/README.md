# Observe Obsidian link resolution

This probe collects Obsidian observations for NORN-422. It contains no accepted resolution expectations yet.
The resolution ADR and Norn implementation follow separately.

[Obsidian 1.14.5 on iPhone](observations/ios-1.14.5/README.md) records the first opening pass and one captured rename.
Those observations include changed inputs and an opening error; they do not certify the original tree or other platforms.

## Install the probe

Copy the contents of `vault/` into an existing Obsidian vault, preserving every relative path.
Use a fresh destination: `artifacts/scratch/norn-422` must not already exist.
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

Unicode filenames are checked in with their intended code points.
Filesystem and sync normalization can change the names actually presented to Obsidian.
Results without actual filename evidence do not settle NFC versus NFD behavior.
A case-insensitive device does not establish case folding on a case-sensitive filesystem.
Record these limitations and repeat affected cases on a suitable filesystem before calling them confirmed.

This is a manual external-application probe, not an automated assertion of current Norn behavior.
NORN-422 remains incomplete until every case has observed read and rename results, with required filesystem evidence.
