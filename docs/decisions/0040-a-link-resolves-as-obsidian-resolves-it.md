---
status: accepted
date: 2026-10-18
---

# 0040 — a link resolves as Obsidian resolves it, by one rule for wikilinks and Markdown links on every root, and norn writes a link only in a form that rule reads back as its target

Norn's links carry Obsidian's semantics: a vault is shared by people working in Obsidian and
agents working through norn, so a link must mean the same thing to both. [ADR
0038](0038-a-link-fix-never-matches-a-link-by-a-rule-obsidian-does-not-apply.md) stated this
for wikilinks. The resolver had drifted from those semantics, and the Obsidian probe in
`tests/fixtures/obsidian-links` (Obsidian 1.14.5 on iOS, cases cited below by id) records
where. Obsidian reached `n422-Éclair.md` from `[[n422-éclair]]` (W09) and NFC and NFD
spellings of one name from each other (W10, W11, M16, M17), where norn folded only ASCII
case, only on a root proven to fold, and normalized nothing. Obsidian opened the image from
`[[photo.png]]` (W13, M25), where norn stripped `.png` and reached `photo.md`. Obsidian
preferred an exact vault path to a deeper suffix (W07, M23), where norn reported the two
ambiguous. Obsidian resolved `./`, `../` and `/` wikilinks (W03–W05), where norn refused them.
Obsidian read an unprefixed Markdown destination by the wikilink's suffix rule (M08 against
M09) and accepted it without `.md` (M02, M13), where norn read it only relative to its
document and required the extension. This decision restores the semantics as one rule.

**A link norn judges healthy opens that target in Obsidian, and a link Obsidian resolves is
never judged broken by norn. Wikilinks and Markdown links resolve by one rule, under one
comparison key on every root: the composed form of a path, lowercased. A target with `./`
or `../` is a path from its document's folder, a target with `/` is a path from the vault
root, and any other target names its exact vault path first and otherwise every path it is
a segment-aligned suffix of. A Markdown document is named with or without `.md`, and no
other extension is ever stripped. Several matches are ambiguous and reported, never
tie-broken. A link norn writes keeps the form it was written in and changes only its path,
judged unique by the same rule that reads it.**

## The contract

- **Parity is the contract, and norn is stricter only compatibly.** A link norn judges
  healthy names the target Obsidian opens, and a link Obsidian resolves is never judged
  broken. Norn may report what Obsidian settles silently: an ambiguity stays a warning
  finding naming its candidates, though Obsidian opens one of them. Norn may suggest a more
  explicit spelling, never require one.
- **One rule reads both families.** A wikilink and a Markdown link differ only in how
  their target text is taken. A wikilink's target is read as written, after its alias and
  anchor are removed; nothing in it is percent-decoded, so `[[n422-space%20name]]` names a
  document spelled with `%20` (W24). A Markdown destination has its angle brackets
  removed, its anchor and query cut off, and each segment percent-decoded (M10, M11, M12).
  A decoded separator stays a character inside its segment. From there the two families
  resolve identically.
- **One comparison key on every root.** Paths and link targets compare by their NFC form
  under Unicode default lowercasing, whatever the root's filesystem does with names.
  Names equal under that key are candidates for each other, and an exact spelling takes no
  precedence: `Note.md` and `note.md` on a root that tells them apart, or NFC and NFD
  spellings of `résumé.md` anywhere (W12), make every link to either ambiguous. The same
  vault names therefore resolve identically on every platform. A root's proven case
  behavior stays a fact the filesystem mechanics need and is no input to resolution. The
  vault schema's ambiguity-ignore globs match under the same key.
- **What a path names.** A Markdown document is named by its vault path with or without
  its `.md` (W01, W02, M01, M02). Any other vault file is named only by its full path. No
  other extension is stripped: `[[photo.png]]` never names `photo.md` (W13, M25),
  `[[v1.2]]` never names `v1.md` (W14, M26), and `[[n422-photo.png]]` and `[[n422-v1.2]]`
  name `n422-photo.png.md` and `n422-v1.2.md` (W15, W16, M24).
- **The form chooses where a target is read from.** These forms hold in both families:
  - A target opening with `./` or `../` is a path from its document's folder (W03, W04,
    M03, M04, M09). One that climbs above the vault root names nothing.
  - A target opening with `/` is a path from the vault root (W05, M07).
  - Any other target names its exact vault path first (W06, W07, M06, M23). Where no path
    matches exactly, it names every path that the target is a right-to-left,
    segment-aligned suffix of (M08). A bare name is the one-segment case.
  - A `vault://` target stays a rooted name, read from the vault root under the same key
    and naming rule, never as a suffix.

  A target given with no document holding it, such as a get's, has no folder, so its
  relative forms name nothing.
- **One match is healthy, several are ambiguous, none is broken.** An ambiguous link
  carries its candidates and is never resolved by a tie-break, though Obsidian opens one
  candidate (W17, A01, A02, M05, M08, W12). A link whose last segment carries an extension
  other than `.md`, and that names no document, is not judged while norn indexes only
  Markdown documents. When other vault files are indexed by name, such a link that names
  no file is broken, under this rule unchanged. Anchors are judged as before: a heading or
  block the target lacks is a missing-anchor finding (W21, W22, M20, M21).
- **A written link keeps its form and changes only its path.** A move cascade, a link
  rewrite and a repair keep a link's family; its form, whether bare, `./` or `../`
  relative, rooted, or `vault://`; whether it spelled `.md`; its alias, anchor and embed
  marker; and a Markdown destination's angle brackets or percent-escaping.
  - A relative or rooted form writes the exact path to the target.
  - A suffix form writes the shortest suffix of the target's vault path that names the
    target alone from that document, judged by the reading rule itself, so a spelling
    unique to norn is unique to Obsidian. The full vault path names the target alone
    unless the target has a twin under the comparison key, and then the link is skipped
    with its reason.
  - A Markdown destination outside angle brackets percent-encodes a space, `%`, and every
    character that would end or change the destination, rather than refusing the
    spelling. This is the escaping Obsidian's own rename wrote (M10).

  Every written link is read back by this rule as its target, or the write is skipped
  with its reason. A link ambiguous before the change is not rewritten.

## Considered options

- **Compare names as the root's filesystem does.** This was the drifted resolver: exact
  bytes on a root that tells spellings apart, ASCII case folded on a root proven to fold.
  Rejected: Obsidian reached case variants beyond ASCII (W09, M15), and a rule taken from
  the filesystem resolves one vault differently on macOS and Linux, where a link must
  mean one thing in every copy of the vault.
- **An exact spelling first, the folded key second.** `[[Note]]` would reach `Note.md`
  silently beside `note.md`. Rejected: no probe shows Obsidian doing so, and the answer
  would again depend on whether a filesystem keeps both names.
- **Obsidian's tie-break.** The probe's ambiguous cases are consistent with preferring the
  document's own folder and then the shortest path (W17, A01, A02, M05, M08). Rejected:
  the order is undocumented and confirmed only by these cases, and a reported ambiguity
  is predictable where a guessed order is not. Reporting is stricter than Obsidian in a
  compatible way.
- **Markdown destinations relative to their document only.** This was the drifted reader.
  Rejected: Obsidian reads an unprefixed destination by the suffix rule (M08 against M09),
  and reserves the relative reading for `./` and `../`.
- **An attachment list of Obsidian's known file extensions.** A dotted name outside the
  list would be a document name, so a missing `[[v1.2]]` would be broken. Rejected: which
  files Obsidian resolves depends on its "Detect all file extensions" setting, so a file
  outside the list would be reported broken where Obsidian opens it. A link that names
  no document stays unjudged until other files are indexed, and indexing them needs no
  change to this rule.
- **Writing the form Obsidian writes.** Obsidian's rename rewrote `../targets/n422-space%20name.md`
  to the bare `n422-space%20renamed.md` (M10). Rejected: parity concerns what a link names,
  not how it is spelled, and preserving the author's form changes the least while naming
  the same target in both.

## Consequences

- A link Obsidian resolves by case, Unicode form, a relative or rooted wikilink form, or a
  Markdown destination without `.md` is no longer reported broken. A link that reached a
  document only by a stripped extension is no longer reported healthy.
- An unprefixed Markdown destination with a same-named document elsewhere in the vault is
  reported ambiguous, where the drifted reader resolved it from its own folder and
  Obsidian opens the copy beside its document (M05). The warning is parity's compatible
  strictness, and `./` names the local copy alone (M04).
- [ADR 0039](0039-an-unquoted-frontmatter-wikilink-is-the-link-it-spells.md)'s last
  consequence no longer describes norn: a link differing from its target only in case is
  no longer reported broken on any root. 0039's repair contract is unchanged: it still
  gives no derived fix by case fold, and such a link no longer needs one.
- The comparison key and the naming rule change what derivation writes for unchanged
  input, so they land behind one derivation-version change ([ADR
  0026](0026-a-derived-store-records-the-derivation-that-wrote-it.md)).
- The probe evidence is one platform: Obsidian 1.14.5 on iOS. Desktop behavior, a
  case-sensitive filesystem, the device's actual Unicode filename spellings, most rename
  rewrites and Obsidian's link-format settings were not observed. This rule is adopted
  for every platform without claiming Obsidian was observed applying each clause there.
  The probe becomes a parity corpus asserting norn's answer for each recorded case.
- While norn indexes only Markdown documents, a link with a dotted name and no matching
  document, such as a missing `[[v1.2]]`, goes unreported, though Obsidian shows it
  unresolved.
