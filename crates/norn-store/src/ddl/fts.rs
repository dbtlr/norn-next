//! The full-text pillar — an external-content FTS5 index over document bodies.
//!
//! `documents_fts` is declared `content='documents'`, `content_rowid='id'`,
//! which means FTS5 keeps the inverted index and **not** a second copy of the
//! text: when a match needs the original — for a snippet, a highlight, or its
//! own integrity check — it reads `documents.body` through the rowid.
//!
//! # What that shape buys, and what it costs
//!
//! It costs a `body` column in `documents`: the document's own text at rest,
//! which is the price of the index having something to point at. The two
//! alternatives each cost more. A contentless index (`content=''`) stores no
//! text, and then no snippet can be produced and rebuilding the index requires
//! walking the vault again — which turns a repairable index into a full tree
//! heal. A default FTS5 table stores the text *twice*, once in its own content
//! table and once in `documents`, and there is no reading under which that is
//! the cheaper option.
//!
//! # The index is maintained by triggers, so it cannot be forgotten
//!
//! Three triggers on `documents` carry it, and they are the only thing that
//! writes to `documents_fts`. A caller cannot maintain the index wrongly
//! because a caller does not maintain it at all: full-text state is
//! transactionally consistent with `documents.body` for every write path that
//! exists now and every one added later.
//!
//! Triggers can be dropped, though, and an external-content index whose
//! triggers are gone drifts silently: the terms it holds still answer a `MATCH`,
//! and they answer it about text the column no longer carries. Detecting that is
//! what FTS5's `integrity-check` **at rank 1** is for — rank 0 checks only that
//! the index is internally well formed, which a desynchronized index is. The
//! store's verification asks for rank 1; see [`crate::Store::verify_integrity`].
//!
//! The update trigger is guarded by `WHEN old.body IS NOT new.body`. A
//! re-derivation that found the body unchanged does no index work at all, which
//! is the warm case, and the guard is what keeps that true rather than merely
//! likely.
//!
//! An external-content delete hands FTS5 the **old** text so it can find the
//! terms to remove, which is exactly what `old.body` is — the reason this is a
//! trigger and not a statement somewhere is that a caller holding the wrong old
//! text corrupts the index silently.
//!
//! # The tokenizer is part of the schema
//!
//! `unicode61 remove_diacritics 2` is pinned rather than defaulted, because the
//! tokenizer decides what a term is: changing it changes every posting list, so
//! it is a DDL edit that the fingerprint catches and a rebuild resolves. `2` is
//! the diacritic handling that treats a composed and a decomposed spelling of
//! the same word as the same word.
//!
//! # The vocabulary is how the index's own contents are read
//!
//! `documents_fts_vocab` is an `fts5vocab` table over `documents_fts` in `row`
//! mode: one row per distinct term, with the number of documents holding it and
//! the number of times it occurs. It reads the inverted index itself.
//!
//! That is what separates it from every other way of asking about the index. A
//! `MATCH` answers a question posed in terms the caller already knows, and
//! `SELECT body FROM documents_fts` reads `documents.body` through the content
//! rowid — so neither of them can state what the index holds where the index and
//! the column disagree. The vocabulary states it directly, and it names no
//! rowid, so two databases carrying the same terms report the same rows whatever
//! order their documents were written in.

pub(crate) fn statements() -> Vec<String> {
    super::fixed(STATEMENTS)
}

const STATEMENTS: &[&str] = &[
    "CREATE VIRTUAL TABLE documents_fts USING fts5(
    body,
    content = 'documents',
    content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 2'
)",
    "CREATE TRIGGER documents_fts_insert AFTER INSERT ON documents BEGIN
    INSERT INTO documents_fts(rowid, body) VALUES (new.id, new.body);
END",
    "CREATE TRIGGER documents_fts_delete AFTER DELETE ON documents BEGIN
    INSERT INTO documents_fts(documents_fts, rowid, body) VALUES ('delete', old.id, old.body);
END",
    "CREATE TRIGGER documents_fts_update AFTER UPDATE ON documents
WHEN old.body IS NOT new.body BEGIN
    INSERT INTO documents_fts(documents_fts, rowid, body) VALUES ('delete', old.id, old.body);
    INSERT INTO documents_fts(rowid, body) VALUES (new.id, new.body);
END",
    "CREATE VIRTUAL TABLE documents_fts_vocab USING fts5vocab(documents_fts, 'row')",
];
