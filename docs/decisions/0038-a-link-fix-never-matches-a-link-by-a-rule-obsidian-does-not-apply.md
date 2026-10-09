---
status: accepted
date: 2026-10-09
---

# 0038 — a repair fix is declared on the constraint it serves, a built-in fix exists only where its answer is unique or a deterministic heuristic's single best, no link fix matches a link by a rule Obsidian does not apply, and every proposed change carries one of three confidence levels

Supersedes [ADR 0036](0036-a-repair-fix-is-declared-on-the-constraint-it-serves.md), whose
contract this decision restates whole with one change: ADR 0036 counted among the derived
fixes a broken link that resolves uniquely after case and separator normalization, and this
decision withdraws it. A wikilink is Obsidian's construct, and a vault norn maintains is one
Obsidian also reads, so how a link resolves is Obsidian's to decide. Obsidian resolves a
wikilink ignoring case and folds nothing else, so a fold a derived fix applies by default
would resolve links as Obsidian does not.

Repair compiles findings into one resolved plan. For each finding it needs an answer — the
value a field should hold, the path a document belongs at — or a reason it has none. Some
answers only the schema's author knows: that `complete` means `done`, what a missing status
defaults to, where a kind of document is filed. Some follow from the vault alone and admit
exactly one answer. Some are a best guess. The question is where each kind of answer comes
from and how a caller tells them apart. **A fix is declared on the constraint it serves,
never in a separate repair section. A built-in fix exists only where its answer is provably
unique, or is a deterministic heuristic's single best answer. No link fix matches a link by
a rule Obsidian does not apply. Every proposed change carries how it was reached —
declared, derived or suggested — and a threshold, derived by default, decides which a plan
admits, identically in preview and apply. Ties never pick, and no model answers any fix.**

## The contract

- **A fix lives on its constraint.** A required field declares its default; a closed set
  declares synonyms, each mapping a written value onto one of its members; an allowed-paths
  constraint declares a route; and a forbidden field declares either a field to rename it
  to or that it is removed. A forbidden field listed without either has no fix. A length
  limit and a field's declared type have no declared fix. Defaults and routes may read the
  clock and the path captures of their own rule's selector, filled once at planning. There
  is no generic repair-rule section.
- **A repair fills one default and cascades none.** A rule default is the declared fix for
  a selected required-missing finding and fills that one field; where the rules selecting
  the document declare differing defaults for it, the finding skips as conflicting
  defaults. A document's fixes compose in finding order, each judged on the state the
  earlier ones composed and skipped where the write check would refuse it. A fix that
  would bring the document under rules requiring fields it lacks — a synonym or a route
  that changes which rules select it — skips, naming those fields and the defaults their
  rules declare, so the caller adds them deliberately. Only `new` and inbox capture fill
  defaults to a fixpoint
  ([ADR 0035](0035-a-schema-rule-selects-documents-by-their-frontmatter.md)).
- **A declared fix is checked with its constraint.** The vault schema refuses at read a fix
  that fails its own rule, among them an untemplated default outside the rule's
  constraints, a synonym whose target is not a member, a route outside its own allowed
  paths, or a rename onto a field the same rule forbids or renames.
- **Built-in fixes are bounded by what can be shown.** A **derived** fix is one whose answer
  is provably the only one: wrapping a single value as a one-item list or unwrapping a
  one-item list, a bare value made a link where it resolves uniquely, a closed-set value
  differing from exactly one member only by case or whitespace, boolean case, or a broken
  link followed to the single live document holding the content a removed target last held. A **suggested** fix
  is a deterministic heuristic's single best answer, without proof: a broken link's most
  similar target above a fixed cutoff, a missing anchor's closest heading, a closed-set
  value's closest member, or an undeclared tag one edit from exactly one declared tag; each
  carries its score. Ambiguous links, date reformatting and coercion of yes/no or numeric
  spellings have no built-in fix.
- **A link fix never matches a link by a rule Obsidian does not apply.** A wikilink carries
  Obsidian's semantics, and norn is stricter than Obsidian only where that never resolves a
  link differently. No derived fix answers a broken link by normalizing its spelling — by
  case, separators or any other fold — because a derived fix applies at the default
  threshold and claims the only answer, which makes the fold a second resolution rule
  beside Obsidian's. A suggested fix may still propose a broken link's most similar target,
  since it carries its score, enters a plan only where the caller admits suggestions, and
  leaves the link broken in norn and in Obsidian alike until a caller chooses it.
- **Three confidence levels, one threshold.** Every proposed change carries its level:
  **declared** for a fix the schema's author wrote, **derived** and **suggested** as above.
  A threshold admits its own level and every stronger one, declared before derived before
  suggested, and defaults to derived. It shapes the plan the same way in preview and in
  apply: a proposal below it becomes a skipped finding carrying the change it would have
  made, never a change.
- **The strongest level decides, and a tie never picks.** Every candidate fix at or above
  the threshold is pooled per finding, and the strongest level present decides. Differing
  answers at that level skip the finding as a tie, naming its candidate fixes under the
  wire's bounded head; a weaker level's differing answer is noted. An ambiguous link is
  always skipped with its resolution candidates. Where several rules contribute to a
  finding, their declared fixes apply only where they agree as filled values and satisfy
  the combined constraint
  ([ADR 0035](0035-a-schema-rule-selects-documents-by-their-frontmatter.md)).
- **A declared fix that fails blocks weaker answers.** A declared fix counts at its level
  even where it fails the combined constraint or the write check: the finding skips with
  every candidate fix, and a derived or suggested answer never silently replaces the
  author's.
- **No model answers a fix.** Derived and suggested fixes are deterministic functions of
  lane-1 pillars and the schema, and read no engine and no model, so the same vault,
  retained tombstones, schema, parameters and clock reading give the same plan
  ([ADR 0024](0024-search-enhancement-is-a-query-surface-tier.md),
  [ADR 0027](0027-link-health-rides-the-changeset.md)).
- **A finding nothing answers is handed back whole.** A finding with no answer at any
  admitted level is skipped with its reason and its decision data — the actual value and,
  for an ambiguous link, its resolution candidates — for the caller to decide.

## Considered options

- **The predecessor line's generic repair rules.** A separate section of rules, each matched
  by finding code, rule name, field and actual value and naming one action — set, remove or
  add a field, or move the document — the first match winning, with literal values only.
  Rejected: each match restated the constraint it served and could drift from it; the
  outcome depended on rule order rather than on the constraint; a literal value could not
  fill from a path or the clock; and with the action held apart from its constraint, the
  schema could not check at read that the action satisfies it.
- **Built-in fixes and the agent only.** No author-declared fixes: built-ins answer what can
  be shown, and the agent decides the rest from the skipped findings. Rejected: the author
  holds answers no inference can prove — a synonym, a default, where a kind of document is
  filed — and without a place to write them once, every occurrence goes back to an agent to
  decide again.
- **Two levels, high and medium.** The predecessor line graded inferred link fixes by
  normalized equality and by similarity. Rejected: it cannot tell an author's answer from an
  inferred one, and an agent validates the two differently.

- **A derived fix for a broken link that resolves uniquely after case and separator
  normalization.** ADR 0036 admitted it. Rejected: Obsidian resolves a wikilink ignoring
  case and never folds separators, so `[[my-note]]` is an unresolved link in Obsidian, and
  rewriting it to `my note.md` by default would give the link a meaning Obsidian does not.
  A link differing from its target only in case already reaches that target in Obsidian, so
  it is not broken by Obsidian's semantics, and making link spellings match file names is a
  question about file naming rather than a repair of a broken link.

## Consequences

- Every operation a repair plan holds names its confidence level, so a caller knows which
  changes to check, and a preview applied later is the plan the threshold shaped.
- A rename onto a field the document already holds is skipped rather than overwriting it.
- Suggested fixes stay out of a plan unless a caller lowers the threshold to admit them.
- Following an out-of-band move reads the content a removed document last held from its
  tombstone, so it answers only where a tombstone recording that content is retained, and a
  derivation from zero, which records no removals, need not reproduce it.
- A broken link differing from a document's name only in case or separators gets no
  derived fix; at the suggested level its most similar target may be proposed with its
  score, and otherwise it is skipped with its decision data.
