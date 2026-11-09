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
//! **The death feed.** A lane-2 consumer that read only the living would keep
//! deriving from a path that is gone. The semantic engine retracts a dead
//! path's rows by reading deaths, its census counts by them, and the host's
//! search margin counts the deaths past the engine's watermark. See the feed
//! section below.
//!
//! **Move-following evidence.** A document moved outside norn leaves a death
//! at its old path and a live row holding the same bytes at its new one, and
//! repair follows a broken link through that move by the death its address
//! names. See [`crate::moved`].
//!
//! **Provenance, checked by verification.** `provenance` says how the death
//! was learned, and it is a closed vocabulary of four: a full tree heal that
//! found the file absent, a watcher removal, an applied plan that deleted it,
//! or a quarantine — a file that is **present** and yields no facts the store
//! can represent. A vault losing documents to the wrong provenance is the
//! shape of a defect that is otherwise invisible, which is why the column is
//! checked against that vocabulary by [`crate::Store::verify_integrity`]
//! rather than trusted, and why a quarantine is not spelled as one of the
//! deaths that mean the path is gone.
//!
//! # One row per path, holding the most recent death
//!
//! `path` is unique and a re-death replaces the row. Every reader wants the
//! latest death and none wants a history: the feed delivers a path's death at
//! the generation it was last recorded, and move-following pools deaths
//! distinct by path.
//!
//! **A tombstone stands only while its path is dead.** The
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
//! The clear loses nothing a reader needs: the live row reaches the feed at a
//! newer generation, and a link naming the path is no longer broken, so
//! move-following has no link there to start from. It also spends the death's provenance:
//! the record of how a path died is deliberately present-tense, and a death
//! the path recovered from is not retained for diagnosis.
//!
//! **A death that never comes back is kept only while a reader needs it**
//! ([ADR 0052]).
//! It stands while the vault's semantic engine or its census has not read
//! past it in the death feed, up to one unread death per live document, or
//! while it is one of the at most two deaths the move-following reads return
//! for a link holding a current broken-link finding. Otherwise the death
//! prune, a claim-bound maintenance unit, removes it, and store meta records
//! the highest generation removed as the death floor in the same transaction.
//! A healthy or ambiguous link keeps no death, and no link keeps a quarantine
//! death, which move-following never reads. The death prune is not built yet,
//! so until it is a tombstone stands until its path derives again.
//!
//! [ADR 0052]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0052-a-death-is-kept-only-while-lane-2-or-a-broken-link-needs-it.md
//!
//! `path` here means the stored spelling. The store never folds case — see
//! [`crate::ddl::documents`] — so on a folding volume one vault *place* can
//! stand live under one spelling and dead under another. The disjointness is a
//! claim about stored paths, not about places.
//!
//! # The suffix keys are stored for a seek
//!
//! A deletion **changes an ambiguity class**: a path that was one of three
//! candidates for `glossary` leaves two behind. The deletion's own
//! transaction re-judges the findings that named that class, deriving the
//! class from the path it deletes — see [`crate::ddl::findings`] — so nothing
//! reads the tombstone for the class afterwards.
//!
//! `suffix_key` and `path_key` are stored for move-following seeks. Both
//! use [`crate::comparison_key`] and are computed with the original path by
//! [`crate::DocumentPath`] in the death's write. `tombstones_suffix_key` and
//! `tombstones_path_key` exclude quarantine deaths because those files still
//! stand on disk. Each index runs on to `path`, so a bounded read returns
//! the first two matching deaths in path order rather than row-id order.
//! The original path indexes retain filesystem paging and identity.
//!
//! `last_content_hash` is nullable, because a death can be learned from a path
//! that is already absent and there is nothing left to hash. A re-death of a
//! path whose hash is already recorded **keeps** the recorded one: the hash is
//! what move-following matches a live holder by, and a second death learned
//! from an absent file carries nothing better to replace it with.
//!
//! # A death is half of the change feed, and this index is its side of it
//!
//! `tombstones_change_feed` orders by `generation` and breaks ties
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
    suffix_key        TEXT    NOT NULL,
    path_key          TEXT    NOT NULL,
    last_content_hash TEXT,
    provenance        TEXT    NOT NULL,
    generation        INTEGER NOT NULL,
    recorded_at       INTEGER NOT NULL
)",
    "CREATE UNIQUE INDEX tombstones_path ON tombstones(path)",
    "CREATE INDEX tombstones_path_key ON tombstones(path_key, path) WHERE provenance <> 'quarantine'",
    "CREATE INDEX tombstones_suffix_key ON tombstones(suffix_key, path) WHERE provenance <> 'quarantine'",
    "CREATE INDEX tombstones_change_feed ON tombstones(
    generation, path, last_content_hash
)",
    "CREATE TRIGGER tombstones_clear_on_derive AFTER INSERT ON documents BEGIN
    DELETE FROM tombstones WHERE path = new.path;
END",
];
