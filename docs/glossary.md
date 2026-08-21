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
A structured statement that vault state violates a rule or cannot be resolved unambiguously. A **place-scoped** finding states that no document is derived at the place it names, so a readable document standing there withholds it; a **document-scoped** finding states something about the document derived at that place and stands beside it.

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

**Dormant carrier**:
An unreached implementation path retained because the layer roadmap names its future consumer. A path with no named consumer is speculative, not dormant.
_Avoid_: Dead code, speculative seam

**Maintainer**:
The single process maintaining one registered vault’s derived state and serving requests against it. Maintainership is exclusive over that derived state — one vault reached by two registrations has two of them, each maintaining its own — and it never restricts any process’s access to the vault’s files.
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

**Index projection**:
A declared unit of derived state naming its inputs, whether it is deterministic, and the one key that invalidates it wholesale. The declaration decides its derivation lane.

**Derivation lane**:
One of two maintenance disciplines for derived state. Lane 1 is deterministic projection of vault documents, committed inside the changeset. Lane 2 is everything else: asynchronous, eventually consistent, and derived from lane-1 records rather than vault files.
_Avoid_: Sync/async indexing (unqualified)

**Engine**:
A lane-2 domain that owns a sidecar database, consumes the change feed, and answers its own query capabilities. An engine never reads vault files.
_Avoid_: Worker, plugin

**Sidecar database**:
A derived database owned by one engine, keyed by content and versions, carrying its own fingerprint and epoch, and rebuildable independently of the main derived store.

**Change feed**:
The ordered, cursor-resumable view of lane-1 change: current rows and tombstones by write generation, projecting fingerprints so a consumer triages before it fetches. It is read as a query over current state, never kept as a log.
_Avoid_: Event queue, event bus

**Store epoch**:
The identity a derived database carries from creation to discard. A feed cursor is valid within one epoch; a new epoch requires a rescan.

**Progressive revalidation**:
Converging derived state after a contract change by re-deriving only what the delta invalidates. Wholesale rebuild is the always-correct floor and the current implementation everywhere.

**Inference firewall**:
The rule that inferred or higher-order derived state answers queries only, and never becomes a finding, a plan, or a repair input.
