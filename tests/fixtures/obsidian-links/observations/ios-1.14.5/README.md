# Obsidian 1.14.5 on iPhone

[results.json](results.json) records 53 manual opening outcomes on iPhone with iOS and Obsidian 1.14.5.
It also records source changes for four links affected by renaming the SPACE target.
These are observations, not accepted resolution rules or automated Norn expectations.

## Input provenance

The [baseline cases](../../cases.json) and [baseline tree](../../vault/) remain unchanged.
[input-state.json](input-state.json) carries six written files and one removed path relative to that tree.
Applying this delta reconstructs the locally captured case-related inputs after the checks.
The delta contains only probe material, including empty documents created outside the probe folder.
It does not inventory unrelated documents, capture device-side filename bytes, or establish when each document was created.

Each opening record carries the link spelling used for that check.
M10, M11, and W23 use the captured post-rename spellings.
W14, M26, W24, W18, and W25 opened documents absent from the baseline.
Their results do not establish that the original missing inputs resolved.
M22 displayed `folder already exist`; its destination is unconfirmed.
The four missing-anchor records combine observed document opening with anchor absence checked against the target content.
They make no claim about warning messages.

The rename before-values come from the checked-in fixture, rather than an immediate source capture before renaming.
The after-values come from file readback; their destinations were checked by opening the links.
The unchanged W24 opens the separate literal-percent-name document.

## Remaining evidence

The exact iOS version, filesystem case sensitivity, device-side Unicode spellings, and link settings are unknown.
These observations cannot separate application normalization from filesystem or sync behavior.
The other 49 cases have no captured rename source text.
All six link-creation setting combinations remain unobserved.
NORN-422 remains incomplete; this record supports the next observations without replacing them with predicted outcomes.
