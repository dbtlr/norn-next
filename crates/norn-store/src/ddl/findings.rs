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
//! keys".
//!
//! The discard is stated as **two ranges rather than an inequality**:
//! `fingerprint < pinned OR fingerprint > pinned`. `<>` is not a predicate an
//! index can answer, so it reads every finding in the table — including on the
//! path where nothing is stale, which is every pin but the ones that changed
//! the schema. Two open ranges are two index seeks over
//! `findings_vault_schema_fingerprint`, and they cost the rows they delete.
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
//! resolution-ladder order, **at most [`crate::CANDIDATE_HEAD`] of them**, and
//! a `CHECK` on `rank` is what makes that structural instead of remembered. The
//! statement builds the bound out of that constant, so the schema and the API
//! cannot come to hold two numbers. `findings.candidates_total` carries how
//! many there really were, and a head longer than the total it claims is
//! refused where the head is written.
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
//!   [`crate::path::class_probe`].
//!
//! The class range is **correct in the no-miss direction, and a superset**: a
//! probe of one class key opens every finding whose class key it prefixes, so
//! nothing a change can invalidate is missed, and a longer-suffix finding in the
//! same class — `[[norn/glossary]]` under `glossary/` — is read even where that
//! particular change cannot have reached it. Re-deciding a finding that was
//! already right is cheap; missing one is a stale finding nothing revisits.
//!
//! A tombstone keeps the same class computable for the same reason: a deletion
//! changes a class, and the class has to stay derivable after the document row
//! is gone. It carries the `path` and nothing derived from it — the class is
//! recomputed from the path, which is how the tombstone is read in the first
//! place.
//!
//! `class_key` is nullable, because not every finding is about resolution. A
//! finding about a frontmatter field violating the vault schema has no
//! ambiguity class, and giving it a synthetic one would put it in the blast
//! radius of every rename that shared a stem with it. Where it is present it is
//! a validated [`crate::ClassKey`]: a class key that is not
//! separator-terminated names a range no probe opens, so the finding would be
//! at rest and permanently invisible to the maintenance that owns it.
//!
//! # Findings outlive their subject, and the class owns their lifecycle
//!
//! A finding is keyed by **path and class**, never by a document row. It has to
//! be: the finding a resolution failure produces is about a path no document
//! has, and a class is invalidated by documents joining or leaving it rather
//! than by the document that cited it changing. So nothing here references
//! `documents`, and no delete cascades into this table — a finding whose subject
//! was deleted is exactly as live as one whose subject was never there, and both
//! are resolved the same way.
//!
//! The way is class-scoped maintenance, in both directions:
//! [`crate::Request::findings_in_class`] reads the class and
//! [`crate::Request::discard_findings_in_class`] empties it. **Discard-then-
//! record is the idempotence story** — re-deriving a class discards it and
//! records what holds now, so there is no dedupe rule to keep and no way for two
//! derivations of one class to leave two copies. Every deletion goes through
//! that verb, which is what makes findings leaving the table counted rather than
//! a side effect of a cascade nobody billed.
//!
//! Nothing here references `links` either. Fact rows are replaced wholesale on
//! every re-derivation, so a reference into them would delete every finding
//! about a document each time that document was re-read. A finding cites a
//! place — path, target, span — and the repair planner re-reads.
//!
//! `detail` is text the caller has already projected, and it travels **in
//! only**. When the wire mints a finding type, the wire owns the code strings
//! and this column stays `TEXT`; `detail` is projected into the store by
//! whatever composed it and is never forwarded back out as a typed shape.

pub(crate) fn statements() -> Vec<String> {
    let mut all = super::fixed(STATEMENTS);
    all.push(finding_candidates());
    all
}

const STATEMENTS: &[&str] = &[
    "CREATE TABLE findings (
    id                       INTEGER PRIMARY KEY,
    vault_schema_fingerprint TEXT    NOT NULL,
    generation               INTEGER NOT NULL,
    kind                     TEXT    NOT NULL,
    severity                 TEXT    NOT NULL,
    path                     TEXT    NOT NULL,
    class_key                TEXT,
    target                   TEXT,
    span_line                INTEGER,
    span_column              INTEGER,
    span_offset              INTEGER,
    candidates_total         INTEGER NOT NULL,
    message                  TEXT    NOT NULL,
    detail                   TEXT,
    CHECK ((span_line IS NULL) = (span_column IS NULL)
       AND (span_line IS NULL) = (span_offset IS NULL))
)",
    "CREATE INDEX findings_class_key ON findings(class_key)",
    "CREATE INDEX findings_path ON findings(path)",
    "CREATE INDEX findings_vault_schema_fingerprint ON findings(vault_schema_fingerprint)",
];

/// The bounded head, with its rank bound taken from the constant the API states
/// it by rather than spelled a second time.
fn finding_candidates() -> String {
    let highest_rank = crate::facts::CANDIDATE_HEAD - 1;
    format!(
        "CREATE TABLE finding_candidates (
    finding INTEGER NOT NULL REFERENCES findings(id) ON DELETE CASCADE,
    rank    INTEGER NOT NULL CHECK (rank BETWEEN 0 AND {highest_rank}),
    path    TEXT    NOT NULL,
    suffix  TEXT    NOT NULL,
    PRIMARY KEY (finding, rank)
) WITHOUT ROWID"
    )
}
