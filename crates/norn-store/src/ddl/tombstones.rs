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
//! learned: a full tree heal that found the file absent, a watcher removal, an
//! applied plan that deleted it, or a rebuild that dropped it. A vault losing
//! documents to the wrong provenance is the shape of a defect that is otherwise
//! invisible.
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
//! # The class columns outlive the document
//!
//! `suffix_key` and `stem` are the same derived forms `documents` carries, kept
//! because a deletion **changes an ambiguity class** and the class has to stay
//! computable once the document row is gone: a path that was one of three
//! candidates for `glossary` leaves two behind, and the findings that named
//! that class are the ones a deletion has to revisit. See
//! [`crate::ddl::findings`].
//!
//! `last_content_hash` is nullable, because a death can be learned from a path
//! that is already absent and there is nothing left to hash.

pub(crate) const STATEMENTS: &[&str] = &[
    "CREATE TABLE tombstones (
    id                INTEGER PRIMARY KEY,
    path              TEXT    NOT NULL,
    suffix_key        TEXT    NOT NULL,
    stem              TEXT    NOT NULL,
    last_content_hash TEXT,
    provenance        TEXT    NOT NULL,
    generation        INTEGER NOT NULL,
    recorded_at       INTEGER NOT NULL
)",
    "CREATE UNIQUE INDEX tombstones_path ON tombstones(path)",
    "CREATE INDEX tombstones_stem ON tombstones(stem)",
    "CREATE INDEX tombstones_generation ON tombstones(generation)",
];
