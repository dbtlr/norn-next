# 0005 — the regression stratum is a named-case registry, and dormancy is venue-labelled

A line of work ended with several hundred records describing how it
misbehaved: tasks, seeds, appendices from a measurement reckoning, and a
running log of annoyances. Almost none of those defects can be tested here
yet, because almost none of their subjects exist — there is no resolver, no
applier, no query surface, no serving host. The choice is therefore not
between testing them and testing them later. It is between **naming them and
losing them**.

This decision records the third test stratum: a registry of named regression
cases, entering as data, each stating what must be true rather than what once
went wrong. `crates/norn/tests/regression/registry.json` holds 97 of them.
Four are carried by tests today. The rest are obligations with an address.

## The contract

**A case is a falsifiable present-tense property, not an incident.** The entry
says what holds — "an existence check executes a select-one-shaped statement",
"the applier's filesystem effect set is exactly the plan's declared effect
set" — and the records it was mined from sit beside it in `sources`. A
measured number appears in a property where it sharpens the claim, because
"9.0 ms of every read" is evidence about what the property forbids. Narrative
does not: what a past investigation found is provenance, and provenance is
what the citation is for.

**Cases are properties and counters over bytes; they are never byte
comparisons.** A regression case asks whether a program's behavior has a
shape — statements executed, rows hydrated, files read, effects declared,
diagnostics reaching every surface. Pinning bytes is a different instrument
with a different failure mode, and this workspace already has it: the coverage
corpus is recorded output judged one command at a time under
[ADR 0001](0001-corpus-activation-gate.md). Bytes pin rendering, which is
layer 6's business. Everything below it is checked by properties, because a
byte comparison over a request that touched the whole tree passes exactly as
happily as one that touched a page of it.

**Every case names a venue, and the venue is the first layer at which a real
test can bind it.** The scale is fixed, and the registry declares it against a
list the harness pins:

| Layer | Venue |
|---|---|
| 0 | fixtures and testkit |
| 1 | substrate |
| 2 | lockdown |
| 3 | queries |
| 4 | mutations |
| 5 | repair |
| 6 | surfaces |

Venue is where a case *first* becomes assertable, not where it will finally
live. Several properties bind at the substrate and are re-asserted at every
layer that consumes it; the entry names the earliest, because the earliest is
when the debt comes due.

**Dormancy is structural, and activation is venue-labelled.** A dormant case
carries no tests and there is no attribute to remove; a case binds when
somebody edits its entry to name the tests that carry it, and the audit holds
those names to functions that really exist and really declare themselves with
`#[test]`. This is the corpus's gate applied to a different question. There,
activation asks whether a recording is *good*. Here, binding asserts that a
property is *carried* — and the audit is what stops that from being a claim
nobody checked.

**Layer 0 is the layer that exists, so dormancy there costs a reason.** A
venue is the first layer that can bind a case, which makes "venue 0, dormant"
a contradiction on its face: the harness is built, so the case is bindable
now. The rule the audit encodes is therefore asymmetric — a dormant case at
any venue above 0 is waiting on a layer, and a dormant case at venue 0 states
in the entry why it is not bound. Those reasons are the sharpest thing in the
file, because each names a specific missing subject: no counter has a
producer, no builder emits SQL, no suite waits on a background condition, the
per-PR flatness pair moves both scale axes at once.

**Five classes are mandatory, and they are pinned in code.** They were
ratified individually before this repository existed, and the audit requires
the registry to mark exactly them:

- `mutation-honors-its-planned-flags` — the applier's effect set is the plan's
  declared effect set, and a dry run leaves the tree unchanged.
- `error-variant-matches-the-operation-reported` — a failure's code comes from
  the semantic domain of the operation actually attempted, as a total table
  under fault injection.
- `one-display-source-per-semantic` — operator-visible text is a function of
  the condition alone, identical across direct and routed, CLI and MCP.
- `unknown-sort-or-projection-keys-never-silently-no-op` — the field universe
  is exact and the diagnostic is a biconditional.
- `forecast-and-apply-are-one-classifier` — apply touches nothing the forecast
  omitted, occurrence by occurrence.

They stay distinct entries even where a broader class covers the same ground,
because they were ratified as themselves. Pinning the names in
`norn-testkit` is what makes removing one cost an edit in two files: the
corpus pins its category lists for the same reason.

**Positive controls are kept, and they are a separate kind.** Three shapes in
the reckoning already satisfied the doctrine — a retention sweep amortized to
once a day, a reverse-index-bounded resolver, a predicate compiled to a SQL
where-clause. A registry of defects alone teaches a reader that everything was
wrong and gives a later change nothing to be measured against. These are held
as `positive-control` so they read as things to keep rather than things to
fix.

**The enforcement meta-classes are layer-0 conventions, and they are the
sharpest cases here.** Ten entries are `enforcement-class`: properties about
how enforcement itself failed. Plan-shape guards asserted against SQL literals
the program never executed. A document redefined a warm-read budget as a
recompute budget, legitimizing the drift it was meant to forbid. An
output-comparison harness could not see internal shape by construction. The
mission property — "a warm request derives nothing" — was never a test. There
was no instrumentation at all, so no counter existed to assert on. And
violations were narrated in the comments of the code committing them. Each of
these binds a convention about how tests are written rather than a behavior of
the program, which is why their venue is 0 and why every one of them carries a
stated reason for still being dormant.

**One entry per distinct falsifiable property.** The three source sweeps
overlap heavily — harness isolation appears in all of them, forecast honesty
in two, the zero-derivation family throughout — so a mined class that restates
a property already named merges into it and adds its citations. What is not
allowed is a mined class going missing: the audit pins the total, so a
deletion that is not deliberate fails.

**The audit judges no property.** It checks that the record holds together:
names unique and kebab-case, a property and at least one citation per entry, a
venue on the scale, the mandatory set exactly the one the harness pins, a
bound case naming tests that exist, a dormant case naming none, and a reason
where the venue already exists. Whether a bound case's tests genuinely carry
its property is a judgment, made when the binding edit is reviewed.

## Consequences

- **A class cannot be forgotten quietly.** The count is pinned in the suite
  and the mandatory names in the harness, so a class leaves the registry by a
  diff that says so. That is the whole reason the registry is data rather than
  a document.
- **A layer landing has a checklist.** The cases at a venue are the properties
  that layer owes, readable by filtering one field. Binding them is part of
  landing it, not a follow-up nobody scheduled.
- **Four cases are bound, and they are all the harness's own.** Two hold the
  process harness — isolated state roots with no ambient environment, and
  bounded, exec-safe children — one holds the fixture generator's content
  volume, and one holds the measurement lanes to proving they measured. They
  are bound because the harness is the only subject that exists, which is the
  same fact the reckoning reached from the other direction: the harness is a
  defect surface, and hermeticity first is what makes every other class
  assertable.
- **A bound case is a reviewed claim, not a matched name.** The audit proves a
  named test exists; it cannot prove the test asserts the property. A binding
  that stretches a test to cover a property it does not carry passes the gate
  and fails review, which is where that judgment belongs.
- **Venue is a claim that can be wrong.** A case filed at layer 4 that turns
  out to be assertable at layer 1 is a mis-filing, and the correction is an
  ordinary edit. Nothing depends on the venue being right in advance; what
  depends on it is that the case is not lost while the answer is unknown.
- **97 is not a floor.** A defect found in this line joins the registry as a
  case with a venue of 0 and a binding, because a defect that has a subject
  has no reason to be dormant.

## Evidence

- Mimir **NORN-a1** — the decisions taken before this repository existed,
  including the three test strata and the five ratified example properties:
  scoped narrowing costs less than whole-vault work, a path glob never voids a
  limit, dead flags are structurally impossible, warm requests assert zero
  derivation counters, and cost is independent of vault size. Each is a case
  here, generalized as the sources recommend — the narrowing property binds
  any narrowing argument on any verb, not the two verbs it was found on.
- The five ratified seeds **NRN-s7, s8, s13, s14 and s20**, carried with their
  full properties as the mandatory set.
- The measurement reckoning's appendices **NRN-a58** through **NRN-a63** — the
  perf baseline, the read path, validate, mainline purity, the mutation path,
  and the synthesis that produced the enforcement classes. The measured terms
  quoted in properties come from these: the ~9.0 ms per-request freshness
  proof at 52–78% of a warm read, 55 KB of findings becoming 8.4 MB across
  three times the documents, ~12,700 statement compilations per mutation, and
  a 30-byte out-of-band edit turning a 9.27 ms read into 356.13 ms.
- The frozen task board — 528 records, of which roughly 230 document a defect
  — and the line's own annoyances log and engineering-learnings notes. Every
  class named in those sweeps is traceable to exactly one entry here.
- [ADR 0001](0001-corpus-activation-gate.md) is the structural precedent: data
  files consumed by one loader, a gate that cannot be bypassed by editing the
  data, and category lists pinned in the harness rather than read from the
  manifest. This registry follows it, and divides from it on authority — a
  recording says what a program did, while a registry entry says what must be
  true.
- [ADR 0004](0004-two-tier-measurement-and-authored-baselines.md) is where the
  counter-versus-clock rule these properties lean on is stated, and the
  reason `budget-is-a-read-budget-not-a-recompute-budget` reads as it does.
- The audit was checked against the shapes it forbids rather than against
  argument alone. A duplicated name, a dropped mandatory flag, a test
  reference naming a function that does not exist, a case deleted without
  moving the total, an unknown field, a dormant layer-0 case with no reason, a
  bound case naming no test, a dormant case naming tests, a venue off the
  scale, a name that is not kebab-case, a renamed venue, an empty property, a
  bound case quietly unbound, and a reference reaching outside the workspace
  each fail it.
