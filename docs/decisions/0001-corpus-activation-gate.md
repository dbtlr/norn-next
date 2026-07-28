# 0001 — the coverage corpus is evidence, and activation is the approval act

A corpus of recorded command invocations lives in `crates/norn/tests/corpus/`,
beside the input trees they ran against, the behavior rulings, and the help
prose recorded with them. **It is evidence with zero authority.** A recording
says what a program did once; it makes no claim about what this program should
do. No recorded case runs until its command is activated, and activation is an
explicit approval act that asks whether the recorded output is *good* — never
whether this program reproduces it.

The corpus enters as data rather than as tests because the alternative is a
suite that passes by agreeing with a recording, which converts an old
program's accidents into this program's requirements. The gate below is what
makes the difference operational rather than aspirational.

> **Amended 2026-07-28** — one clause of this decision was replaced: a case no
> longer depends on a generator reproducing its tree. The original wording is
> left standing wherever it appears, each occurrence carrying the amendment
> beneath it, and [Amendments](#amendments) records the change in full.

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
by a recorded ruling. The help prose follows its own ladder, decided at the
same moment: a command whose shape survives takes the recorded prose verbatim
and then reviews it; a command whose shape changed uses it as a basis for
iteration; a command it no longer fits regenerates from the voice standard;
and a command that is deleted takes its prose with it.

**Every command the binary had at the recording pin sits in exactly one of
three disjoint categories.** The audit reconciles the set against the binary's
own recorded command list, so a command cannot sit outside every category
unnoticed, and it refuses a command that appears in two:

| Category | Meaning |
|---|---|
| `activatable` | Has recorded cases, and can be approved. |
| `unseeded` | Carries no behavior at the pin, so there is nothing to approve and no activation path exists. |
| `unrecorded` | Has behavior at the pin, but no case exercises it, so its contract is authored with no evidence at all. Each entry states why. |

`activated` is not a fourth category. It is the subset of `activatable` whose
cases run — empty today — and a command must be activatable before it can be
activated.

**Nine commands are unseeded:** `init`, `completions`, `cache`, `config`,
`self-update`, `serve`, `service`, `audit`, `manpage`. Whether each should
exist is a question for the verb charter, which derives the verb set from
callers and jobs. The list is pinned in `norn-testkit` rather than read from
the manifest, and the audit requires the manifest to name exactly those nine —
so an unseeded command cannot be given an activation path by editing the
manifest, which would otherwise be a two-line change.

**One command is unrecorded:** `vault`. It has behavior at the pin and appears
in the help catalog and in one ruling, but no case invokes it. Naming the gap
is the point: its contract is authored from scratch. This list is pinned in
the harness too, for the same reason — otherwise moving a command here would
quietly take its cases off the gate.

**A case is judgeable only against the tree it ran on.** Each case names a
generator profile and seed, and `fixtures.json` records the tree each of those
produced, file by file. The generator did not cross into this repository and
is re-derived with different realism knobs, so a case cannot be activated
until the generator reproduces its recorded tree exactly. Without that, a
difference in output reads as a defect in this program when it is drift in the
generator.

↳ **Amended 2026-07-28.** The first sentence stands and the rest is replaced.
**A case materializes its tree from its own recording**, because the manifest
now carries every entry's exact bytes — text verbatim, non-UTF-8 bytes
base64-encoded — and no entry records a length in place of contents. The
profile and seed are the recording's *name*, not an instruction to regenerate
it, and no generator is in the path. The clause's original worry is not
dropped but answered more directly: a generator that never runs cannot drift
into a case's input.

**Recordings hold placeholders where a value could not reproduce, and the two
kinds are kept apart.** The extraction's own masks — a minted telemetry id, a
root-dependent plan hash, a wall-clock stamp — are declared per case in
`volatile_masks`, and each was proven necessary by re-running the whole
catalog and requiring byte equality. The one substitution kept from the
recording harness, the vault-root path, is declared separately in
`normalizations`. There are no others: a harness substitution that erased a
value or deleted a line the binary really emitted existed to make two
programs' outputs comparable, and each destroys information a recording exists
to carry, so none is applied.

Two rules make a declaration mean something, and the audit enforces both in
both directions. **A mask fires only on a nonempty value**: an empty field is
a fact about the invocation — a write-free path mints no telemetry id — so it
is recorded verbatim and the case declares no mask. And **a substitution is
declared where it changed bytes, not where it was applied**: the vault-root
substitution runs on every invocation but is declared only by the cases whose
recordings carry `<VAULT>`. A placeholder in the bytes with no declaration
reads as output the program produced; a declaration with no placeholder points
at nothing.

## What the recordings say about exit codes

This is a property of the evidence, not a contract of this program. **The exit
contract binds where the verb charter authors it**; what follows describes only
the classification the recordings were made under, so a reader of a recorded
`exit_class` knows what it meant.

The recordings were classified tri-state: `0` success, `1` operational failure
(a well-formed invocation that could not be carried out), `2` usage error (an
argv the parser could not accept). One specialization applied to commands that
write, keyed on whether any write landed: `2` was a refusal that left the vault
untouched, `1` a partial apply. **That specialization took precedence over the
usage class** — a writing command rejecting a malformed payload was recorded as
a refusal, not a usage error, because the load-bearing fact was that nothing
had been written. Whether any of this is right for this program is a question
the verb charter answers.

## Consequences

- **A skipped case is a debt with a name.** The count of unactivated commands
  is readable from one file, and no command reaches "done" by having its cases
  quietly not run.
- **The corpus cannot grow authority by accident.** Cases enter the workspace
  as inert data files consumed by one loader; nothing renders the help prose,
  and nothing asserts against a ruling until its command is activated.
- **A ruling can be stranded.** A ruling attached only to commands with no
  activation path will never come up for judgment, so the harness reports that
  set explicitly rather than letting it sit unread. `PD-134` is the whole set
  today.
- **The runner is not built.** Executing a case needs a fixture tree the
  generator can reproduce and a composition root that does something; neither
  exists. The suite fails loudly naming both if a command is activated first.

  ↳ **Amended 2026-07-28.** Executing a case needs its tree written out from
  the recorded manifest and a composition root that does something. The first
  is now only code to write — the bytes are all present — so the composition
  root is the one thing genuinely missing, and the suite names that.

## Evidence

- Mimir **NORN-a1** — the decisions taken before this repository existed,
  including the three test strata and the activation gate. It names eight
  unseeded commands in prose and enumerates nine; the enumeration governs, and
  the list above is the corrected one.
- The recording pin `76af2c3b` on branch `rewrite/0017` of the `norn-legacy`
  line, cited by every corpus data file. 158 cases, 11 fixture trees, 56
  behavior rulings, and 45 help entries were recorded from it under the
  environment described in `crates/norn/tests/corpus/environment.json`.
  Recording was a one-time act performed in that checkout: only data crosses
  into this workspace, and the tool that performed it was deliberately not
  retained. What carries instead is the procedure, written down in that
  environment file and in the corpus README — and an independent
  reimplementation from that prose reproduced the corpus byte for byte, which
  is the evidence the rules are stated completely.

## Amendments

### Amendment 1, 2026-07-28 — recorded cases materialize from their manifests

**What changed.** The clause making activation wait on a generator reproducing
a case's tree is withdrawn. In its place:

- **A recorded case materializes its tree from its recorded manifest.** Every
  entry carries its exact bytes, so writing each entry out at its path
  reconstructs the tree the case ran on.
- **The fixture generator binds its own profiles and nothing else.** It is not
  asked to reproduce a recorded tree, and a difference between what it produces
  and what a manifest records is not a defect in either.
- **Activation therefore depends on the recorded manifests and the runner**,
  and never on generator reproduction.

**Why it changed.** The original clause rested on manifests being an incomplete
record — a name plus a partial description, from which a tree could only be
recovered by re-running the program that made it. That was true when the
decision was taken: 11 of 422 entries carried a byte *length* where their
contents belonged, one binary asset per manifest, so no manifest could
reconstruct its own tree. Completing the record removes the reason for the
clause rather than overruling it. The 11 entries were re-recorded from the
recording pin, each verified byte-identical across two independent generations,
and every entry now carries `content` or `content_base64`; recording a length
is no longer a legal state, which the loader enforces at parse time.

The clause's underlying worry — that a difference in output could read as a
defect in this program when it is drift in a generator — is not dropped. It is
answered more completely than the clause managed: a generator that is never
consulted cannot drift into a case's input.

**What it touched.** The two contract statements above, each amended in place;
`crates/norn/tests/corpus/fixtures.json` and its note;
`crates/norn/tests/corpus/README.md`; the `TreeEntry` and `FixtureManifest`
types and the audit in `norn-testkit`; and the corpus suite's account of what
activation still needs. **ADR 0002** states the generator side of the same
contract.

Nothing else in this decision changes. The corpus is still evidence with zero
authority, activation is still the per-command approval act that judges whether
recorded output is *good*, and no recording has gained any claim on what this
program should do.
