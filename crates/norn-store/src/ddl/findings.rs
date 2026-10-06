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
//! the schema. Two open ranges are two index seeks over either index leading
//! with the fingerprint, and they cost the rows they delete.
//!
//! `findings_fingerprint_kind_nocase` carries the kind and the path after the
//! fingerprint, which is the direction a find's finding part reads: the paths a
//! kind of finding stands over under the active fingerprint, one covering seek
//! on the pair and the paths off the index. The fingerprint leads rather than
//! the kind so that a statement naming kinds alone — a subject's discard, a
//! walk's page of subjects — is not offered a kind-led index in place of the
//! path it seeks.
//!
//! # A path's findings stand in the order of the links they are about
//!
//! `ordinal` is the ordinal of the link a finding is about in the document at
//! its path, and `NULL` for a finding about the document itself. Within one
//! path, findings stand by that ordinal, and a finding about the document
//! stands ahead of every finding about one of its links: a validate, which
//! pages kind by kind, orders a kind's findings at a path by ordinal and then
//! id, and a get and the find findings column, which page one document's
//! findings, order them by ordinal, then kind, then id. So the order of a
//! path's findings is a function of the links they are about, and the id, which
//! is the order they were filed in, decides only between findings that share
//! everything before it.
//!
//! **One link holds at most one finding.** The kinds a finding about a link
//! is filed under exclude one another, so `findings_one_per_link` holds
//! `(fingerprint, path, ordinal)` unique wherever `ordinal` is set, and a
//! second finding about a link is refused where it is written. Two findings at
//! one path under one fingerprint therefore share a position only where both
//! are about the document, and the id decides only between those. The index is
//! partial, so no read that does not name a set ordinal can seek it.
//!
//! **The indexes order `position`, never `ordinal`.** `position` is a
//! generated column, `coalesce(ordinal, -1)`, which no write names, and every
//! reader reads a finding's position off it. The `-1` is
//! [`DOCUMENT_POSITION`], which the statement is built from. SQLite
//! orders `NULL` before every integer in an index and an `ORDER BY`, but a
//! row-value comparison that reaches a `NULL` term is `NULL`, and a seek
//! applies the comparison to the rows it reaches. So a continuation from a
//! finding about the document, bound on `ordinal`, would pass no finding at its
//! path; a first page bound below every ordinal would pass none about a
//! document. `position` holds the same order with no `NULL` in it, so every
//! page seeks it as a row value.
//!
//! # A validate reads the findings in `(kind, path, position, id)` order, off three indexes
//!
//! `validate` pages the findings standing under the active fingerprint in
//! `(kind, path, position, id)` order, one kind at a time, with the path in the
//! answer's path order on every root — `path COLLATE NOCASE`, then `path`
//! bytewise, which makes the order total over two paths that fold together.
//! Every index here carries the row id as its last column, so each order below
//! continues by the id with nothing sorted:
//!
//! - `findings_fingerprint_kind_nocase` is `(fingerprint, kind, path COLLATE
//!   NOCASE, path, position)`: one kind's findings in `(path COLLATE NOCASE,
//!   path, position, id)` order, sought past a page's position on all four and
//!   bounded by a path part's folded range, and a page a document part drives
//!   seeks it at each matched document's path.
//! - `findings_fingerprint_kind_severity_nocase` is `(fingerprint, kind,
//!   severity, path COLLATE NOCASE, path, position)`. A request narrowed to one
//!   severity seeks it in the same order within that severity, so the findings
//!   of another severity cost nothing; and it covers a summary, whose tallies
//!   group by `(kind, severity)` in the index's own order, a path part's
//!   folded range bounding each cell's seek, and a document part's seek at
//!   each matched document's path.
//! - `findings_path` is `(path, fingerprint, position, kind)`: the findings
//!   standing at one path, in `(position, kind, id)` order, which is how a get
//!   pages a document's findings and a find's findings column reads a
//!   document's head and stops at its ceiling. Its leading `path` is also every
//!   subject read and discard's seek, and a walked-scope prune's bytewise page
//!   of subjects.
//!
//! - `finding_rules_fingerprint_rule_kind_nocase` and
//!   `finding_rules_fingerprint_rule_kind_severity_nocase` are the first two
//!   with the rule a finding cites one column further in, over
//!   `finding_rules` — see below — so a request selecting one rule pages and
//!   tallies the findings citing it in the same order by the same seeks.
//!
//! A statement a document part drives joins each matched document to its
//! findings on the path compared folded and bytewise. The folded equality is
//! implied by the bytewise one, and it is what lets the seek at a matched
//! document's path run down the `NOCASE` column of the two indexes above. The
//! bytewise equality is what matches a finding to the one document at its
//! exact path, where the root tells `a.md` and `A.md` apart.
//!
//! # A finding cites its rules by one set, held once per fingerprint
//!
//! A finding judged against the schema rules cites every rule contributing to
//! the constraint it breaches (ADR 0035). `findings.rule_set` names the set by
//! one integer, so a finding row's bytes do not grow with the rules it cites.
//! `rule_sets` holds each set once per vault-schema fingerprint as its
//! canonical spelling — the names in byte order, each once, as a JSON list —
//! which is both what a write finds the set by again, under
//! `rule_sets_fingerprint_rules`, and what a reader reads the set's names back
//! from ([`crate::rule_set`]); the names are held nowhere else as a set. A set
//! is held under the fingerprint the findings citing it were derived under,
//! and it stands exactly as long as a finding cites it:
//! `findings_collect_rule_set` deletes it as the last finding citing it is
//! deleted, by whichever discard — a changeset's, a caller's or a schema
//! pin's — so a store maintained through any history holds the sets a store
//! derived from zero holds. The citation is a foreign key, and
//! `findings_rule_set` is the index the collection's check for another citer
//! and the foreign key's own check both seek, so collecting a set costs the
//! findings citing it.
//!
//! **Selecting by rule reads a row per finding and rule.** A finding citing
//! several rules is found by any of them, so `finding_rules` holds one row
//! per `(finding, rule)` pair, beside a copy of the finding's fingerprint,
//! kind, severity, path and position: the key its two indexes order by, in
//! the findings' own order, ending in the finding's id. The copy is taken from
//! the finding's own row as the rule row is written, and the rows cascade with
//! the finding, so a discard takes them whole. The store's verification holds
//! the copy equal to the finding and the rows' names equal to the set the
//! finding cites, so the rules a validate selects a finding by are the rules
//! its row reports. These rows grow with the rules a finding cites, which is
//! what selecting by any one of them costs; the finding's own row does not.
//!
//! No finding cites a rule or carries a value until rule judgment in
//! derivation files one (NORN-358): today only the store's own suite writes
//! these tables and columns, through [`crate::FindingFacts::rules`] and
//! [`crate::FindingFacts::value`].
//!
//! # The offending value is a head, at rest as on the wire
//!
//! A finding about a value carries it as `value_head`, `value_bytes` and
//! `value_hash`: the value's first [`norn_wire::VALUE_HEAD_BYTES`] cut at a
//! character boundary, its whole length, and the SHA-256 of the whole of it,
//! taken where the finding is written. `CHECK`s built from the wire's
//! constant hold the head to the bound and to the value, a value within the
//! bound whole, a longer one cut within three bytes of the bound, the hash to
//! the SHA-256 spelling, and the three present together or absent together,
//! so a value of any length costs a finding the same bytes and every head at
//! rest is one the wire reads. The combined expectation a value breached — a closed set, a
//! limit, the allowed paths — is never stored with it: it is a function of
//! the rules the finding cites.
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
//! table: it is a range scan over the suffix key the root probes — raw, or
//! folded by ASCII case — keyed by a class the finding is in, which costs the
//! class rather than the vault and returns the class as it stands rather than
//! as it stood. The scan is the one resolver's ([`crate::TargetClass`]), so a
//! finding's class is the class a find's `resolves` part reads on that root.
//!
//! The class-bearing findings are the link-health findings the store judges
//! inside every changeset ([`crate::health`]): a finding about a
//! suffix-addressed link is filed under the classes its keys open, and a
//! change to any of them discards it and re-decides the link. Every finding
//! the host files is about its own subject and carries no class key.
//!
//! A finding's class keys are spelled in the key space its root probes, which
//! the store's path order selects, and a changed path names its class in that
//! same space — see [`crate::IncrementOutcome::affected_classes`] — so
//! maintenance reaches every finding filed in the store. A finding filed under
//! a key outside that space is refused where it is written, and a class or a
//! class probe from another space is refused where it is read.
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
//! - A finding's classes are the same form built from the target it is about,
//!   one per reduction of that target: `[[glossary]]` gives `glossary/`,
//!   `[[norn/glossary]]` gives `glossary/norn/`, `[[v1.2]]` gives `v1.2/` *and*
//!   `v1/`. They are rows in `finding_classes`, one per pair.
//! - So the findings a changed path affects are the ones holding a class key
//!   that path's class key prefixes — one prefix range over
//!   `finding_classes(class_key)`, joined back by finding id — and the
//!   candidates of each are one prefix range over `documents(suffix_key)`. Both
//!   bounds come from [`crate::Request::class_probe`], in the key space the
//!   store's path order selects.
//!
//! The class range is **correct in the no-miss direction, and a superset**: a
//! probe of one class key opens every finding holding a class key it prefixes, so
//! nothing a change can invalidate is missed, and a longer-suffix finding in the
//! same class — `[[norn/glossary]]` under `glossary/` — is read even where that
//! particular change cannot have reached it. Re-deciding a finding that was
//! already right is cheap; missing one is a stale finding nothing revisits.
//!
//! # Membership is a set, because the two reductions are disjoint
//!
//! A written target whose leaf carries a dot reduces both ways and reads both
//! ranges, and those ranges do not nest: `.` is `0x2e` and `/` is `0x2f`, so
//! under `BINARY` collation `notes.tar/` sorts *before* `notes/` rather than
//! inside it. A single-valued class column could therefore hold only one of the
//! two, and the no-miss direction would be false for every finding about such a
//! target — a finding about `notes.tar` filed under `notes/` alone is invisible
//! to the class `archive/notes.tar.gz` is in, and a finding about `v1.2` filed
//! under `v1/` alone is invisible to the class `notes/v1.2.md` is in. Both are
//! findings that no maintenance ever revisits, which is exactly what class
//! scoping exists to prevent.
//!
//! So `finding_classes` holds one row per `(finding, class)` pair, and a finding
//! is reached through **any** class it is in. The rows cascade from the finding,
//! so a class discard takes the whole finding — its other memberships included —
//! and the read side asks for each finding once however many of its classes a
//! probe opens. `finding_classes_class_key` is the index the class direction
//! seeks through; the primary key is the finding direction, and being `WITHOUT
//! ROWID` is what makes the pair the row rather than a payload beside one.
//!
//! # Path keys are the other key space, in a table of their own
//!
//! A path-addressed link — `[x](dir/t.md)`, `[[vault://Notes]]` — reaches
//! documents through the exact paths it spells, and no class range reaches it:
//! deleting `dir/t.md` changes the class `t/`, and `dir/t.md` is not in that
//! range. So a finding about such a link is maintained by those paths:
//! `finding_paths` holds one row per `(finding, path)` pair, each a validated
//! [`crate::PathKey`] spelled as the link index holds the link's path, in the
//! key space the store's path order selects. A changeset discards every
//! finding holding the path key of a path it writes or kills — one equality
//! seek of `finding_paths_path_key` per path, joined back by finding id — and
//! a rename is a death of the old path and a write of the new, so both paths'
//! findings go.
//!
//! **The two key spaces never share a table.** A class discard is a prefix
//! range, and the path key `glossary/x.md` sorts inside the range the class
//! `glossary/` opens, so a single membership table would let a class discard
//! take a finding no member of its class can change. `finding_paths` is
//! reached by equality alone, and a path key never ends in the separator, so
//! no path key is a class key either.
//!
//! The path-keyed findings are the link-health findings about a
//! path-addressed link, filed under the paths it spells, which the store
//! re-decides when a changeset writes or kills one of them. Every finding the
//! host files carries no path key.
//!
//! A tombstone keeps the same class computable for the same reason: a deletion
//! changes a class, and the class has to stay derivable after the document row
//! is gone. It carries the `path` and nothing derived from it — the class is
//! recomputed from the path, which is how the tombstone is read in the first
//! place.
//!
//! A finding's class set may be **empty**, because not every finding is about
//! resolution. A finding about a frontmatter field violating the vault schema has
//! no ambiguity class, so it has no row here at all, and giving it a synthetic one
//! would put it in the blast radius of every rename that shared a stem with it.
//! Every key that is present is a validated [`crate::ClassKey`]: a class key that
//! is not separator-terminated names a range no probe opens, so the finding would
//! be at rest and permanently invisible to the maintenance that owns it.
//!
//! # Findings outlive their subject, and the class owns their lifecycle
//!
//! A finding is keyed by **path, class and path key**, never by a document row.
//! It has to be: the finding a resolution failure produces is about a path no
//! document has, and a class is invalidated by documents joining or leaving it
//! rather than by the document that cited it changing. So nothing here
//! references `documents` and no document delete reaches a finding — one whose
//! subject was deleted is exactly as live as one whose subject was never there,
//! and both are resolved the same way. The cascades that do exist run the other
//! way, from a finding to the candidate, class and path rows that are parts of
//! it.
//!
//! The way is class-scoped maintenance, in both directions:
//! [`crate::Request::findings_in_class`] reads the class, and a changeset that
//! changes a path in it empties it inside its link-health re-decision, a chunk
//! of findings at a time by row id. **Discard-then-record is the idempotence
//! story** — re-deriving a class discards it and records what holds now, so
//! there is no dedupe rule to keep and no way for two derivations of one class
//! to leave two copies. Every deletion is a counted discard — a changeset's
//! subject, class and path discards, a caller's subject discard, and a schema
//! pin's — which is what makes findings leaving the table counted rather than a
//! side effect of a cascade nobody billed.
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

use crate::write_path::WriteStatement;

pub(crate) fn statements() -> Vec<String> {
    let mut all = super::fixed(RULE_SET_STATEMENTS);
    all.push(findings());
    all.extend(super::fixed(STATEMENTS));
    all.push(collect_rule_set());
    all.push(finding_candidates());
    all
}

/// Where a finding about the document stands among its path's findings: the
/// `position` the table generates for a `NULL` ordinal, below every link's
/// ordinal. A reader binds it as the position a cursor naming a finding about
/// the document resumes after, and as the least position a first page opens
/// at.
pub(crate) const DOCUMENT_POSITION: i64 = -1;

/// The hexadecimal digits a SHA-256 is spelled in, after its `sha256:` prefix.
const SHA256_HEX_DIGITS: usize = 64;

/// The rule sets a finding cites, ahead of the findings that reference them.
const RULE_SET_STATEMENTS: &[&str] = &[
    "CREATE TABLE rule_sets (
    id                       INTEGER PRIMARY KEY,
    vault_schema_fingerprint TEXT    NOT NULL,
    rules                    TEXT    NOT NULL
)",
    "CREATE UNIQUE INDEX rule_sets_fingerprint_rules ON rule_sets(vault_schema_fingerprint, rules)",
];

/// The trigger that collects a rule set when the last finding citing it goes,
/// its body [`WriteStatement::CollectRuleSet`] with the deleted finding's
/// citation in place of the parameter, so the plan bar over that statement is
/// a plan of what the trigger runs.
fn collect_rule_set() -> String {
    format!(
        "CREATE TRIGGER findings_collect_rule_set AFTER DELETE ON findings
    WHEN old.rule_set IS NOT NULL
BEGIN
    {};
END",
        WriteStatement::CollectRuleSet
            .sql()
            .replace("?1", "old.rule_set")
    )
}

/// The findings table, with the position of a finding about the document
/// taken from [`DOCUMENT_POSITION`] and the value head's bound from
/// [`norn_wire::VALUE_HEAD_BYTES`] rather than spelled a second time.
///
/// The value checks are the wire's [`norn_wire::ValueHead`] grammar as far
/// as a byte length can state it: a head within the bound and the value; a
/// value within the bound carried whole; a longer one cut no shorter than a
/// character boundary forces, which is within the longest UTF-8 character's
/// bytes ([`char::MAX_LEN_UTF8`]) less one of the bound; and the hash spelled
/// as a SHA-256.
fn findings() -> String {
    let value_head_bytes = norn_wire::VALUE_HEAD_BYTES;
    let shortest_cut = value_head_bytes - (char::MAX_LEN_UTF8 - 1);
    let hash_spelling = format!("sha256:{}", "[0-9a-f]".repeat(SHA256_HEX_DIGITS));
    format!(
        "CREATE TABLE findings (
    id                       INTEGER PRIMARY KEY,
    vault_schema_fingerprint TEXT    NOT NULL,
    generation               INTEGER NOT NULL,
    kind                     TEXT    NOT NULL,
    severity                 TEXT    NOT NULL,
    path                     TEXT    NOT NULL,
    target                   TEXT,
    span_line                INTEGER,
    span_column              INTEGER,
    span_offset              INTEGER,
    candidates_total         INTEGER NOT NULL,
    message                  TEXT    NOT NULL,
    detail                   TEXT,
    ordinal                  INTEGER CHECK (ordinal >= 0),
    position                 INTEGER GENERATED ALWAYS AS (coalesce(ordinal, {DOCUMENT_POSITION})) VIRTUAL,
    rule_set                 INTEGER REFERENCES rule_sets(id),
    value_head               TEXT    CHECK (length(CAST(value_head AS BLOB)) <= {value_head_bytes}),
    value_bytes              INTEGER CHECK (value_bytes >= length(CAST(value_head AS BLOB))),
    value_hash               TEXT    CHECK (value_hash GLOB '{hash_spelling}'),
    CHECK ((span_line IS NULL) = (span_column IS NULL)
       AND (span_line IS NULL) = (span_offset IS NULL)),
    CHECK ((value_head IS NULL) = (value_bytes IS NULL)
       AND (value_head IS NULL) = (value_hash IS NULL)),
    CHECK (value_bytes > {value_head_bytes}
        OR length(CAST(value_head AS BLOB)) = value_bytes),
    CHECK (value_bytes <= {value_head_bytes}
        OR length(CAST(value_head AS BLOB)) >= {shortest_cut})
)"
    )
}

const STATEMENTS: &[&str] = &[
    "CREATE INDEX findings_fingerprint_kind_severity_nocase ON findings(
    vault_schema_fingerprint, kind, severity, path COLLATE NOCASE, path, position
)",
    "CREATE INDEX findings_fingerprint_kind_nocase ON findings(
    vault_schema_fingerprint, kind, path COLLATE NOCASE, path, position
)",
    "CREATE INDEX findings_path ON findings(path, vault_schema_fingerprint, position, kind)",
    "CREATE UNIQUE INDEX findings_one_per_link ON findings(
    vault_schema_fingerprint, path, ordinal
) WHERE ordinal IS NOT NULL",
    "CREATE INDEX findings_rule_set ON findings(rule_set) WHERE rule_set IS NOT NULL",
    "CREATE TABLE finding_classes (
    finding   INTEGER NOT NULL REFERENCES findings(id) ON DELETE CASCADE,
    class_key TEXT    NOT NULL,
    PRIMARY KEY (finding, class_key)
) WITHOUT ROWID",
    "CREATE INDEX finding_classes_class_key ON finding_classes(class_key)",
    "CREATE TABLE finding_paths (
    finding  INTEGER NOT NULL REFERENCES findings(id) ON DELETE CASCADE,
    path_key TEXT    NOT NULL,
    PRIMARY KEY (finding, path_key)
) WITHOUT ROWID",
    "CREATE INDEX finding_paths_path_key ON finding_paths(path_key)",
    "CREATE TABLE finding_rules (
    finding                  INTEGER NOT NULL REFERENCES findings(id) ON DELETE CASCADE,
    rule                     TEXT    NOT NULL,
    vault_schema_fingerprint TEXT    NOT NULL,
    kind                     TEXT    NOT NULL,
    severity                 TEXT    NOT NULL,
    path                     TEXT    NOT NULL,
    position                 INTEGER NOT NULL,
    PRIMARY KEY (finding, rule)
) WITHOUT ROWID",
    "CREATE INDEX finding_rules_fingerprint_rule_kind_severity_nocase ON finding_rules(
    vault_schema_fingerprint, rule, kind, severity, path COLLATE NOCASE, path, position, finding
)",
    "CREATE INDEX finding_rules_fingerprint_rule_kind_nocase ON finding_rules(
    vault_schema_fingerprint, rule, kind, path COLLATE NOCASE, path, position, finding
)",
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
