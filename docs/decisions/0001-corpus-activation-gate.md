# 0001 — the coverage corpus is evidence, and activation is the approval act

A corpus of recorded command invocations lives in `crates/norn/tests/corpus/`,
beside the behavior rulings and help prose recorded with it. **It is evidence
with zero authority.** A recording says what a program did once; it makes no
claim about what this program should do. No recorded case runs until its
command is activated, and activation is an explicit approval act that asks
whether the recorded output is *good* — never whether this program reproduces
it.

The corpus enters as data rather than as tests because the alternative is a
suite that passes by agreeing with a recording, which converts an old
program's accidents into this program's requirements. The gate below is what
makes the difference operational rather than aspirational.

## The contract

**Activation is per command, and the manifest is the record.** A command's
cases run when its name appears in `activated` in
`crates/norn/tests/corpus/activation.json`, and not otherwise. Dormancy is
structural: there is no attribute to remove and no environment variable to
set, because approval is a judgment a person makes, and the manifest is where
that judgment is written down.

**Activating a command answers four questions, in order.**

0. **The goodness preamble.** Is the recorded output independently good,
   judged against the surface being re-derived rather than against the
   recording? A recording that is not good is retired by a recorded ruling and
   the contract is authored fresh — activation is not the only exit.
1. **The shape gate.** Does the output shape change? If not, the cases enter
   as they stand.
2. **Mechanical migration.** If the shape changes mechanically, the recorded
   bytes are rewritten to the new shape and the command is activated.
3. **Semantic divergence.** If the change is semantic, the ruling that decides
   it is recorded first, and activation follows the ruling.

**A cross-command inconsistency is decided once, as a class ruling**, and
swept across every command it touches. One grammar, not per-command drift; the
commands a class ruling sweeps are activated together.

**Behavior rulings attach per command and are re-judged at activation.** A
ruling becomes contract only on an affirmative judgment against the re-derived
surface. A command the verb charter deletes takes its rulings with it, retired
by a recorded ruling. The same ladder governs the help prose: a surviving
command's prose is raw material, a modified command's prose is a starting
point, and a deleted command's prose dies with it.

**The exit contract is tri-state**, and every recorded case carries its class:

| Code | Class | Meaning |
|---|---|---|
| `0` | `ok` | The request succeeded. |
| `1` | `operational` | A well-formed invocation that could not be carried out. |
| `2` | `usage` | An argv the parser could not accept. |

The mutation commands specialize the failure split, keyed on whether any write
landed: `2` is a `refusal` that left the vault untouched, and `1` is a
`partial-apply`. The class is part of what activation judges — a command whose
failures are classified wrongly is not activated by rewriting the recording.

**Nine commands are unseeded and have no activation path at all:** `init`,
`completions`, `cache`, `config`, `self-update`, `serve`, `service`, `audit`,
`manpage`. They carry no behavior at the recording pin, so there is nothing to
approve. Whether each should exist is a question for the verb charter, which
derives the verb set from callers and jobs. The audit in
`norn-testkit`'s corpus module refuses any corpus that offers one of them as
activatable, so the list cannot be widened by editing the manifest.

## Consequences

- **A skipped case is a debt with a name.** The count of unactivated commands
  is readable from one file, and no command reaches "done" by having its cases
  quietly not run.
- **The corpus cannot grow authority by accident.** Cases enter the workspace
  as inert data files consumed by one loader; nothing renders the help prose,
  and nothing asserts against a ruling until its command is activated.
- **Recordings hold placeholders, not machine facts.** Absolute vault paths,
  minted telemetry ids, root-dependent plan hashes and wall-clock stamps are
  replaced by named placeholders, and each case names the placeholders that
  fired on it. Recording a value that cannot reproduce would pin noise.
- **The runner is not built.** Executing a case needs a fixture vault from the
  deterministic generator and a composition root that does something; neither
  exists. The suite fails loudly naming both if a command is activated first.

## Evidence

- Mimir **NORN-a1** — the decisions taken before this repository existed,
  including the three test strata and the activation gate. It names eight
  unseeded commands in prose and enumerates nine; the enumeration governs, and
  the list above is the corrected one.
- The recording pin `76af2c3b` on branch `rewrite/0017` of the `norn-legacy`
  line, cited by every corpus data file. 158 cases, 56 behavior rulings, and 45
  help entries were recorded from it under the environment described in
  `crates/norn/tests/corpus/environment.json`. Recording was a one-time act
  performed in that checkout: only data crosses into this workspace.
