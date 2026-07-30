//! The findings pillar — structured statements that vault state is wrong or
//! cannot be resolved, and the one table in the schema the vault schema shapes.
//!
//! # Schema-dependent, and keyed for it
//!
//! `vault_schema_fingerprint` is the invalidation key. Whether vault state
//! violates a rule is a question the vault schema asks, so a finding derived
//! under one schema says nothing under another: a schema edit discards exactly
//! the rows whose key is not the new fingerprint, and touches no parse-fact
//! table. That is the whole of "a schema edit re-derives exactly the tables it
//! keys" — one predicate over one indexed column.
//!
//! # `generation` is what a repair plan cites
//!
//! A repair plan is compiled against findings as they stood, and it says so by
//! carrying the generation it was planned at. The applier can then tell a plan
//! that describes the current world from one that describes a world two
//! derivations ago. What a repair does **not** do is trust the snapshot: it
//! reads the live ambiguity class and re-decides, so the generation is evidence
//! about the plan's age rather than an input to its outcome.
//!
//! # The bounded head of five, at rest as on the wire
//!
//! `finding_candidates` holds a finding's candidates in deterministic
//! resolution-ladder order, **at most five of them**, and `CHECK (rank BETWEEN
//! 0 AND 4)` is what makes that structural instead of remembered.
//! `findings.candidates_total` carries how many there really were.
//!
//! A bounded payload has to be bounded at rest too, or the bound is a rendering
//! step that a second consumer forgets. A vault with a four-hundred-document
//! ambiguity class stores five candidate rows for a finding about it and the
//! number four hundred.
//!
//! # Full candidate enumeration is a query, and it is indexed
//!
//! The head is a head *because* the full list stays reachable. It is not a
//! table: it is a range scan over `documents(suffix_key)` keyed by the
//! finding's `class_key`, which costs the class rather than the vault and
//! returns the class as it stands rather than as it stood.
//!
//! # Ambiguity classes, and why maintenance is scoped by class
//!
//! **A finding about resolution belongs to an ambiguity class, never to a
//! document.** Consider three documents — `docs/norn/glossary.md`,
//! `notes/glossary.md`, `archive/glossary.md` — and a link written
//! `[[glossary]]`. The finding is that the target has three candidates. Adding
//! a fourth `glossary.md` anywhere in the vault invalidates it; so does
//! deleting one; and *neither change touches the document the link was written
//! in*. Maintenance scoped per changed document would therefore leave stale
//! findings behind, and maintenance scoped to the whole vault would re-derive
//! everything on every keystroke.
//!
//! The scope that is both correct and cheap is the **affected ambiguity
//! class**, and the segment-reversed path encoding is what makes it computable
//! and indexable:
//!
//! - A document's class key is its `stem` followed by a separator —
//!   `docs/norn/glossary.md` gives `glossary/` — and every suffix target that
//!   can address it is a prefix of its `suffix_key`, `glossary/norn/docs/`.
//! - A finding's `class_key` is the same form built from the target it is
//!   about: `[[glossary]]` gives `glossary/`, `[[norn/glossary]]` gives
//!   `glossary/norn/`.
//! - So the findings a changed path affects are the ones whose `class_key`
//!   starts with that path's class key — one prefix range over
//!   `findings(class_key)` — and the candidates of each are one prefix range
//!   over `documents(suffix_key)`. Both bounds come from
//!   [`crate::path::class_probe`], and neither reads a row outside the class.
//!
//! A tombstone carries the same two derived forms for the same reason: a
//! deletion changes a class, and the class has to stay computable after the
//! document row is gone.
//!
//! `class_key` is nullable, because not every finding is about resolution. A
//! finding about a frontmatter field violating the vault schema has no
//! ambiguity class, and giving it a synthetic one would put it in the blast
//! radius of every rename that shared a stem with it.
//!
//! # What a finding cites, and what it does not
//!
//! `path` is text rather than only a document reference, because a finding
//! outlives the row it is about: a target that resolves to nothing names a path
//! no document has. `document` is the reference where one exists, so that
//! deleting a document takes its findings with it.
//!
//! Nothing here references `links`. Fact rows are replaced wholesale on every
//! re-derivation, so a reference into them would delete every finding about a
//! document each time that document was re-read. A finding cites a place — path,
//! target, span — and the repair planner re-reads.

pub(crate) const STATEMENTS: &[&str] = &[
    "CREATE TABLE findings (
    id                       INTEGER PRIMARY KEY,
    vault_schema_fingerprint TEXT    NOT NULL,
    generation               INTEGER NOT NULL,
    kind                     TEXT    NOT NULL,
    severity                 TEXT    NOT NULL,
    path                     TEXT    NOT NULL,
    document                 INTEGER REFERENCES documents(id) ON DELETE CASCADE,
    class_key                TEXT,
    target                   TEXT,
    span_line                INTEGER,
    span_column              INTEGER,
    span_offset              INTEGER,
    candidates_total         INTEGER NOT NULL,
    message                  TEXT    NOT NULL,
    detail                   TEXT
)",
    "CREATE INDEX findings_class_key ON findings(class_key)",
    "CREATE INDEX findings_path ON findings(path)",
    "CREATE INDEX findings_document ON findings(document)",
    "CREATE INDEX findings_generation ON findings(generation)",
    "CREATE INDEX findings_vault_schema_fingerprint ON findings(vault_schema_fingerprint)",
    "CREATE TABLE finding_candidates (
    finding INTEGER NOT NULL REFERENCES findings(id) ON DELETE CASCADE,
    rank    INTEGER NOT NULL CHECK (rank BETWEEN 0 AND 4),
    path    TEXT    NOT NULL,
    suffix  TEXT    NOT NULL,
    PRIMARY KEY (finding, rank)
) WITHOUT ROWID",
];
