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
