---
status: accepted
date: 2026-10-05
---

# 0035 — a schema rule selects documents by what their frontmatter says, a path narrows it or adds an area's requirements, and no path makes a folder a container

A vault schema has declared each field once for the whole vault — its type, whether it is
required, and the closed set of values it admits — and declared folders by a path and a
description. A constraint that holds for some documents and not others has no home there:
a task needs a status from its own closed set, a meeting note does not. The question is
what decides which documents a constraint governs. **A schema rule selects documents by
what their frontmatter says. A path glob narrows a rule to an area or, alone, adds that
area's requirements and defaults; no path makes a folder a container, so what a document
is stays decided by its frontmatter, and the vault schema has no folder declaration.**
Rules are named and additive, and the rules that select one document combine into one
constraint per field.

## The contract

- **One name, one type.** The vault schema's field declarations give each key one type, and
  optionally one shape, across the whole vault. Every other constraint — required,
  forbidden, a closed set of values, a length limit, the paths a document may stand at —
  lives on a schema rule. A value that does not read as its key's declared type or shape is
  a finding about that value, whichever rules select the document.
- **A rule is named and selects by frontmatter first.** Each rule has a name unique in the
  vault schema, an optional description, and an optional severity, `warning` where it
  declares none. It selects a document by frontmatter values — each key compared as a
  find's equality part compares it, a tag under the tag fold, a typed key by its typed
  value, otherwise exactly as written, against one value or any of a list, and every key
  must match — by a path glob whose named path captures each bind one path segment, and by
  path globs it excludes. A rule with no selector applies to the whole vault; a rule that
  selects by path alone applies to the area its glob names. There is no presence test, no
  negation, no regular expression and no nested key.
- **A path adds requirements to an area; it does not say what stands there.** A path-only
  rule holds every document in its area to its constraints, its defaults answer required
  fields missing there, and its path captures feed those defaults. It does not classify the
  documents it selects, nest inside another rule, inherit from one or override one, and a
  document moved out of its area leaves its requirements behind while the rules its
  frontmatter selects travel with it. The paths a rule allows are a constraint it states,
  and a rule's description carries what a folder's did. The vault schema declares no
  folders.
- **Defaults cascade only where a document is created.** Where `new` or inbox capture
  creates a document, rule defaults fill the required fields still missing and rules are
  matched again on what they filled, until no rule brings in another; co-selecting rules
  whose defaults for a field disagree refuse the creation. A repair cascades nothing: a
  rule default is the declared fix for one selected required-missing finding and fills
  that one field; co-selecting rules whose defaults for it disagree skip it; and a fix that
  would bring in required fields the document lacks skips, naming those fields and their
  defaults
  ([ADR 0036](0036-a-repair-fix-is-declared-on-the-constraint-it-serves.md)).
- **Rules are additive and combine per field.** No rule overrides another: a document is
  held to every rule that selects it at once. Each field is judged once per constraint kind
  against the combined constraint of those rules — the intersection of their closed sets,
  required where any requires it, forbidden where any forbids it, the smallest length
  limit, and a path allowed only where every contributing rule allows it. **There is
  one finding per path, field, constraint kind and offending value**: a list field is
  judged element by element, each offending element its own finding naming that element.
  A finding cites every contributing rule and is reported at the highest of their
  severities.
- **A conflict between rules is named, never silently resolved.** Where a combined
  constraint is empty — a field one rule requires and another forbids, closed sets with no
  member in common where the field is required or holds a value, or allowed paths with none
  in common — the finding says the rules conflict and names every contributing rule. A
  conflict the selectors alone make unavoidable, between a vault-wide rule and any other or
  between rules whose selectors are identical, refuses the vault schema at read, naming
  every contributing rule in name order.
- **Rule judgment reads one document.** Whether a rule selects a document, and what the
  combined constraint finds in it, is a pure function of that document's path and
  frontmatter and the vault schema, so its findings are derived in the document's own
  changeset.

## Considered options

- **The predecessor line's selector rules as they were.** Its selector vocabulary is kept —
  frontmatter values, path globs with captures, excluded paths — but each of its rules
  judged a document alone and filed its own finding, so a field two rules constrained was
  reported once per rule, each against an expectation the other might contradict, and no
  fix could be judged against what the document must actually hold. Combining rules per
  field gives one finding a fix can answer and a write can be judged by.
- **Document types keyed by a discriminator.** Each document names its type in one key, and
  the vault schema declares per-type constraints. Rejected here: it fixes one key and one
  type per document, where a vault classifies by several keys and a document can answer to
  several rules at once, and a rule selecting on one frontmatter key already expresses a
  type's constraints. Whether document types earn a place of their own is a separate
  question, to be opened from the problem they solve.
- **Folder-scoped rules.** Constraints attach to a declared folder and every document under
  it inherits them. Rejected: where a document stands would decide what it is, so a move
  would silently change its kind and a document filed in the wrong place would be judged as
  the wrong kind rather than as misplaced; nested folders need an inheritance and override
  order; and the folder declaration would answer where a document may stand a second way,
  beside the paths a rule allows. A path-only rule keeps the one use — an area's own requirements and
  defaults — without the container.

## Consequences

- The vault schema's folder declarations leave its grammar, and a field declaration keeps
  only its type and shape; required and closed-set constraints move onto rules. The grammar
  changes in place at version 1 with both migration ladders empty; a vault schema in the
  earlier shape refuses at read as an unknown key does.
- The same bytes can be valid at one path and in breach at another, because path selectors
  and the paths a rule allows depend on where a document stands. A move that carries a document byte for
  byte is judged again at its destination.
- Selecting findings by rule name reads a mapping from each finding to every rule it cites,
  since one finding can cite several.
- A vault schema read without refusal can still yield rules-conflict findings, where a
  conflict depends on a document's values or path; only a conflict visible from the
  selectors alone refuses at read.
