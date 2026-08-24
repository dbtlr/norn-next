//! `tombstones` — the record of a document's death.
//!
//! A document deletes **hard**: the row leaves `documents`, the cascade takes
//! every fact row derived from it, and all of it happens in one transaction.
//! Nothing is soft-deleted, so no read has to filter for the living. What
//! survives is this: the path, the hash it was last seen at, the generation
//! the death was recorded at, and where the news came from.
//!
//! # Why a dead path is worth a row
//!
//! **Ordering, against a filesystem that reports out of order.** A watcher may
//! deliver a change for a path after the heal that pruned it, and a request may
//! arrive between the two. Without a record, the late event finds no document
//! and has to guess whether it is describing a file that is gone or a file
//! nothing has derived yet — and the two want opposite responses. With one, the
//! event compares its own hash and generation against the death and answers
//! without racing anything.
//!
//! **Reasoning, for heal and doctor.** `provenance` says how the death was
//! learned, and it is a closed vocabulary of four: a full tree heal that found
//! the file absent, a watcher removal, an applied plan that deleted it, or a
//! quarantine — a file that is **present** and yields no facts the store can
//! represent. A vault losing documents to the wrong provenance is the shape of a
//! defect that is otherwise invisible, which is why the column is checked
//! against that vocabulary by [`crate::Store::verify_integrity`] rather than
//! trusted, and why a quarantine is not spelled as one of the deaths that mean
//! the path is gone.
//!
//! # One row per path, holding the most recent death
//!
//! `path` is unique and a re-death replaces the row. The comparison a late
//! event makes is against the *most recent* death, so keeping a history of
//! deaths would mean every such comparison started with a `MAX(generation)`
//! over rows nothing else reads.
//!
//! **A tombstone stands exactly while its path is dead.** The
//! `tombstones_clear_on_derive` trigger removes the same-path tombstone when a
//! document row is inserted — see [`crate::Change::Upsert`] — so at rest the
//! two pillars are disjoint: a path holds a document row, or a tombstone, or
//! neither, never both. The trigger is the one maintainer, for the reason the
//! full-text triggers are: every writer of `documents` upholds the invariant
//! by construction, and a second maintainer in code could disagree with it.
//! `INSERT` alone is enough — an update means a live row already stood, and a
//! live row means no tombstone stands to clear. [`crate::Store::verify_integrity`]
//! checks the disjointness at rest rather than trusting the trigger.
//!
//! The clear loses nothing a late event needs, because the live row carries a
//! newer generation and the current hash — a better comparison basis than the
//! death it replaced. It also spends the death's provenance: the record of how
//! a path died is deliberately present-tense, and a death the path recovered
//! from is not retained for diagnosis. Retention for a path that dies and
//! never comes back — when its tombstone has outlived the disorder it was
//! recorded to survive — is not decided here.
//!
//! `path` here means the stored spelling. The store never folds case — see
//! [`crate::ddl::documents`] — so on a folding volume one vault *place* can
//! stand live under one spelling and dead under another. The disjointness is a
//! claim about stored paths, not about places.
//!
//! # The class outlives the document, and it is recomputed rather than stored
//!
//! A deletion **changes an ambiguity class**, and the class has to stay
//! reachable once the document row is gone: a path that was one of three
//! candidates for `glossary` leaves two behind, and the findings that named that
//! class are the ones a deletion has to revisit. See [`crate::ddl::findings`].
//!
//! The class comes from `path`, which is the column a tombstone is read by, and
//! [`crate::DocumentPath`] derives it on the way out. Storing the derived forms
//! beside it would put one fact in two homes for a scan no statement performs.
//!
//! `last_content_hash` is nullable, because a death can be learned from a path
//! that is already absent and there is nothing left to hash. A re-death of a
//! path whose hash is already recorded **keeps** the recorded one: the hash is
//! the comparison basis a late event needs, and a second death learned from an
//! absent file carries nothing better to replace it with.
//!
//! # A death is half of the change feed, and this index is its side of it
//!
//! A lane-2 consumer that read only the living would keep deriving from a path
//! that is gone. `tombstones_change_feed` orders by `generation` and breaks ties
//! on `path`, then carries `last_content_hash`, so a page of the death feed is a
//! seek into the index and a walk along it — the row itself is never read and
//! nothing sorts.
//!
//! The tie-break is what makes a position a position: one changeset stamps every
//! death it records with one generation, so a cursor holding a generation alone
//! would either repeat the changeset it stopped inside or skip the rest of it.
//!
//! What it costs is the same shape the document side pays, one column narrower:
//! the path and the last content hash duplicated per tombstone, and a key led by
//! `generation`, so a re-recorded death moves its entry rather than updating it
//! in place. See [`crate::ddl::documents`].
//!
//! The two feeds are merged in that one order, and the disjointness above
//! keeps a stored path out of both at once: the insert that revives a path
//! takes its tombstone with it, same-changeset deaths included. **Should a
//! merge ever present both at one position, the document row outranks the
//! death** — a death deletes the document row, so a document standing is the
//! later fact — but the schema keeps that rule vacuous.

pub(crate) fn statements() -> Vec<String> {
    super::fixed(STATEMENTS)
}

const STATEMENTS: &[&str] = &[
    "CREATE TABLE tombstones (
    id                INTEGER PRIMARY KEY,
    path              TEXT    NOT NULL,
    last_content_hash TEXT,
    provenance        TEXT    NOT NULL,
    generation        INTEGER NOT NULL,
    recorded_at       INTEGER NOT NULL
)",
    "CREATE UNIQUE INDEX tombstones_path ON tombstones(path)",
    "CREATE INDEX tombstones_change_feed ON tombstones(
    generation, path, last_content_hash
)",
    "CREATE TRIGGER tombstones_clear_on_derive AFTER INSERT ON documents BEGIN
    DELETE FROM tombstones WHERE path = new.path;
END",
];
