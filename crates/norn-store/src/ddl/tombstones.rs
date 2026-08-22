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
//! **A tombstone is a record of a death, never a claim about the present.** A
//! path that has died and been recreated has both a document row and a
//! tombstone, and `documents` is the one that says what is there now. Retention
//! — when a tombstone has outlived the disorder it was recorded to survive — is
//! not decided here.
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
//! The two feeds are merged in that one order, and they are not disjoint —
//! a path that died and was written again stands in both. **At one position the
//! document row outranks the death.** A death deletes the document row, so a
//! path holding both at one generation is a path that changeset killed and then
//! wrote, and `documents` holds its end state.

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
];
