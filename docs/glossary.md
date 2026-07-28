# Glossary

The domain terms of `norn`, and what each one means here specifically.

**Terms enter with the contracts that earn them** — a term is added when a landed contract
depends on the distinction it draws, not in advance of one. Where a term is not here,
[`architecture.md`](architecture.md) carries the vocabulary in context, and
[`decisions/`](decisions/README.md) does the same as each ADR lands.

## The coverage corpus

Earned by [ADR 0001](decisions/0001-corpus-activation-gate.md), whose contract turns on the
distinctions below.

- **Coverage corpus** — recorded command invocations carried from a frozen line as inert
  data: argv in, bytes and an exit class out, against a recorded input tree. Evidence with
  zero authority; it never states what this program should do.
- **Activation** — the approval act that lets one command's recorded cases run. It judges
  whether the recorded output is *good* against the surface being re-derived, never whether
  this program reproduces the recording.
- **Activatable** — a command that has recorded cases and so can be approved. Not the same
  as running: the manifest's `activated` list, not this category, decides that.
- **Unseeded** — a command that carried no behavior at the recording pin. There is nothing
  to approve and no activation path exists; whether it should exist at all is the verb
  charter's question.
- **Unrecorded** — a command that had behavior at the pin but that no case exercises. Its
  contract is authored with no evidence, and the category exists to say so out loud.
- **Exit class** — how a recording's exit code was classified when it was made: success,
  operational failure, usage error, or — on a command that writes — a refusal that left the
  vault untouched, or a partial apply. A property of the evidence; this program's exit
  contract is authored with the verb charter.

## Fixture generation

Earned by [ADR 0002](decisions/0002-fixture-determinism-and-calibration.md), whose contract
turns on the distinctions below.

- **Fixture profile** — a named, compile-time constant naming a document count and one
  setting per realism knob. With a seed it is the *whole* input to generation, which is what
  makes a generated tree reproducible; a setting reachable from outside the pair would break
  that.
- **Realism knob** — one dial of how closely a generated tree resembles a real collection:
  body length, ambiguity classes, link density, directory shape, non-Markdown clutter. Each
  is a seeded draw, so turning one changes the tree without making it unpredictable.
- **Ambiguity class** — a set of documents sharing one file-name stem in different
  directories. Its size is **`k`**: resolving the bare stem names all `k`, so a class larger
  than a bounded candidate list is what proves the bound truncates rather than merely fits.
- **Calibration envelope** — the checked-in ranges a generated tree's shape statistics are
  expected to land in, each carrying its reasoning. Authored rather than measured, and moved
  only by a reviewed edit — which is what makes recalibration a deliberate act rather than
  something a generator does to itself.

## Boundary enforcement

Earned by [ADR 0003](decisions/0003-boundary-enforcement-harness.md), whose contract turns
on the distinctions below.

- **Architecture gate** — the per-PR test that reads the workspace dependency graph and
  holds it to the allowlist: every observed edge permitted, every permitted edge between two
  present crates observed, every member a crate the map names.
- **Edge-held** — carried by an edge the allowlist withholds, so a violation cannot compile.
- **Lint-held** — carried by a symbol-level rule the dependency graph cannot express, with
  the crates that legitimately own the effect carving it out at the use site.
- **Review-held** — carried by a judgment a person makes, because no rule expresses it yet.
  Named as such rather than left implied: a review-held invariant rots quietly.
- **Pending rule** — a named lint rule whose subject does not exist yet, recorded in the
  mapping so that configuring it is a deliberate edit rather than a discovery.
- **Size-independence pair** — one operation run against two fixture profiles of different
  scale, with the counters compared name by name. Counts, never clocks.
