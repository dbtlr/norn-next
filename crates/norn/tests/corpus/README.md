# The coverage corpus

Recorded command invocations from a frozen line, and the material recorded
alongside them. **All of it is evidence with zero authority.** A recording
says what a program did once. It makes no claim about what this program should
do, and nothing here is a specification.

Every file cites the pin it was recorded from. Recording was a one-time act
performed in that checkout: no binary, no harness code, and no build artifact
crossed into this repository — only data.

The contract this data serves is [ADR 0001](../../../../docs/decisions/0001-corpus-activation-gate.md).
The loader and the gate are `norn-testkit`'s `corpus` module; the suite is
`../corpus.rs`.

## Nothing here runs

The cases that run are exactly the cases of the commands named in `activated`
in `activation.json`. That list is empty.

Dormancy is structural. There is no attribute to remove and no environment
variable to set, because approving a command's recorded output is a judgment a
person makes, and the manifest is where that judgment is written down.
**Activation asks whether the recorded output is good** — never whether this
program reproduces it. A recording that is not good is retired by a recorded
ruling and the command's contract is authored fresh.

`activation.json` sorts every command the binary had at the pin into one of
four categories, and the audit reconciles that set against the binary's own
recorded command list so none can go uncategorized. The nine `unseeded`
commands are additionally pinned in the harness, so no manifest edit can give
one an activation path.

## The files

| File | What it holds |
|---|---|
| `activation.json` | The gate: which commands run, which could, which never can, and which have no evidence at all. |
| `cases/<command>.json` | The recorded invocations for one command — the unit of activation. |
| `unseeded/<command>.json` | Recordings that reach a command with no behavior at the pin. Held apart so they are outside the activatable set by construction. |
| `fixtures.json` | The input tree each `(profile, seed)` produced, file by file. |
| `environment.json` | The environment every recording ran under, and what the placeholders in recorded output mean. |
| `behavior-ledger.json` | Behavior rulings, attached to the commands whose cases they cover. |
| `help-prose.json` | Per-command help prose. Rendered nowhere. |

A file that is not `.json` is ignored by the loader, which is why this README
can sit here.

## Three kinds of substitution, deliberately spelled differently

Recorded text is verbatim apart from these, and conflating them loses
information.

1. **`<TRACE_ID>`, `<PLAN_HASH>`, `<TIMESTAMP>` — extraction masks.** Applied
   by the extraction, and declared per case in `volatile_masks`. Each was
   proven necessary by re-running the whole catalog and requiring byte
   equality between runs. A mask fires **only on a nonempty value**: an empty
   field is a fact about the invocation — a write-free path mints no telemetry
   id and computes no plan hash — so it is recorded verbatim and the case
   declares no mask.
2. **`<VAULT>` — an inherited normalization.** The recording harness replaced
   the vault's absolute path before the extraction saw the bytes. A case
   declares it in `normalizations`, never in `volatile_masks`.
3. **`{{VAULT_ROOT}}` — an input token.** It appears in a case's
   `plan_template`, not in recorded output: the recording substituted the
   vault path for it *before* invoking. It is spelled unlike the other two on
   purpose, because it moves in the opposite direction.

## Reading a case

`argv` in, `recorded` out, against the tree `fixture` names. A mutating case
also carries `recorded_tree_delta` — what the invocation did to the vault.

`recorded.exit_class` is the classification **the recordings were made
under**, not this program's exit contract; ADR 0001 says what each class
meant and where the real contract lands.

Paths in `behavior-ledger.json` — `source_file`, and each ruling's
`source_decision` — point inside the recording checkout, **not this
repository**. The two namespaces overlap, so reading one as a local path
resolves to the wrong document.

## A case is only judgeable against its tree

`fixtures.json` exists because `(profile, seed)` does not determine a tree.
The generator that produced these trees did not cross into this repository and
is re-derived with different realism knobs, so **a command cannot be activated
until the generator reproduces its cases' recorded trees exactly.** Without
that, a difference in output reads as a defect in this program when it is
drift in the generator.
