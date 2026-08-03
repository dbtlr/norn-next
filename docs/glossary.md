# Glossary

Canonical language for `norn`. Product language names concepts visible in the system's behavior; engineering language names project-specific concepts used to build and verify it.

## Product language

**Vault**:
A directory tree of Markdown files that Norn treats as durable source truth; other files may coexist without becoming vault documents.
_Avoid_: Collection

**Vault document**:
A Markdown file in a vault whose authored frontmatter and body are part of the source truth.
_Avoid_: Note, file (when the distinction matters)

**Vault schema**:
User-authored rules defining the valid structure and values of vault documents.
_Avoid_: Doctrine, schema (unqualified)

**Derived state**:
Rebuildable state computed from vault documents. It is never the source of truth.

**Trust state**:
The explicit assessment of whether a vault entry's derived state may safely answer requests.

**Vault registration**:
Inclusion of a vault in the host's durable serving set. A vault included in that set is a registered vault.

**Plan**:
A declarative proposed vault mutation, including its intended effects and the conditions under which they remain safe.

**Forecast**:
A report of what a plan would do against a particular observed vault state.

**Refusal**:
A resolved outcome in which Norn performs no requested mutation because safety, trust, or preconditions are not satisfied.
_Avoid_: Error, failure (when the distinction matters)

**Finding**:
A structured statement that vault state violates a rule or cannot be resolved unambiguously.

**Quarantined document**:
A vault document Norn cannot decode, which therefore contributes no derived state and is named by a finding carrying its path and the cause, except while a readable document occupies the same rendered place. Quarantine is scoped to the one document; the rest of the vault is derived and served.
_Avoid_: Skipped, ignored (when the distinction matters)

**Resolution target**:
A document reference interpreted through one resolution grammar across every Norn surface.
_Avoid_: Path, stem (when referring to the complete reference)

**Resolution candidate**:
A vault document that could satisfy a resolution target.
_Avoid_: Candidate (unqualified)

**Ambiguity class**:
The set of vault documents satisfying the same resolution target when that target does not identify exactly one document.

## Engineering language

**Store schema**:
The internal structure and version of Norn's rebuildable derived state.
_Avoid_: Schema (unqualified), vault schema

**Changeset**:
A group of document derivations and deaths submitted together for one atomic application.
_Avoid_: Batch, diff

**Vault entry**:
The host's runtime state for one attached vault, including the resources and trust state used to serve it.

**Vault attachment**:
The lifecycle that associates a vault with a vault entry and establishes trustworthy derived state before requests are served.
_Avoid_: Attach (as a noun)

**Coverage corpus**:
Historical command invocations carried as inert input and output evidence with no authority over Norn's behavior.

**Corpus activation**:
The explicit human approval that permits one command's coverage-corpus cases to run after judging their recorded behavior independently.
_Avoid_: Activation (unqualified)

**Enforcement posture**:
How a boundary invariant is currently carried: by a withheld dependency edge, an executable lint, or an explicit review judgment. A planned rule is not enforcement until it exists.

**Review-held invariant**:
An invariant carried by an explicit human judgment because no executable rule currently expresses it.

**Maintainer**:
The single process maintaining a registered vault’s derived state and serving requests against it. Maintainership is exclusive; it never restricts any process’s access to the vault’s files.
_Avoid_: Owner, single-owner

**Shadow**:
A staged, unpublished copy of one document write. A shadow is never a vault document and is never surfaced by any Norn reading surface; publication is atomic, and a shadow that outlives its write attempt is inert.
_Avoid_: Temp file (when the distinction matters)

**Post-state identity**:
The identity a landed write reports for what it published, by which Norn later recognizes its own writes.

**Own-write ledger**:
The record of post-state identities Norn’s writes produced, consulted to distinguish its own filesystem events from foreign ones.

**Size-independence pair**:
The same operation run at two fixture scales with structural counters compared by name, demonstrating that cost does not grow with vault size.

**Authored threshold**:
A checked-in range, ceiling, or baseline changed only by a reviewed edit with stated grounds. Observations may justify changing it but never redefine it automatically.

**Regression stratum**:
The set of regression obligations retained from prior defects as properties that the current line must continue to satisfy.

**Regression case**:
One named, falsifiable property in the regression stratum. Multiple historical incidents may contribute provenance to the same case.

**Binding venue**:
The earliest system layer at which a real test can carry a regression case.
_Avoid_: Venue (unqualified)

**Regression binding**:
The deliberate association of a regression case with the tests that carry its property. A case is dormant before that association and bound afterward.
_Avoid_: Activation, bound (unqualified)
