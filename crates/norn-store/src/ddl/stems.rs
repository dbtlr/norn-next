//! Stem trigrams are a lane-1 projection under ADR 0047: their only input is
//! the document path, a lane-1 pillar, so the inference firewall stands. The
//! invalidation key is the derivation version; a change to derivation moves
//! that version and rebuilds from zero. Storage is declared in indexed
//! trigrams: one row per distinct window of the folded, padded stem, at most
//! folded stem length + 1 rows per document.
//!
//! A document's path is its row's identity, so its rows are written once, when
//! the increment creates the row, and an edit at a standing path writes none.
//! A death takes them through the cascade, which seeks the `(document,
//! trigram)` index, so a rename is a death and an insert.
//!
//! The changeset maintains this projection because SQL cannot fold NFC or
//! compute the padded set. SQLite FTS5's trigram tokenizer was evaluated and
//! rejected: its fold is not NFC followed by per-character lowercase, it pads
//! nothing, and its postings cannot be probed with a bounded LIMIT per trigram
//! or counted as rows written.

pub(crate) fn statements() -> Vec<String> {
    super::fixed(&[
        "CREATE TABLE document_trigrams (
            trigram TEXT NOT NULL,
            document INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            PRIMARY KEY (trigram, document)
        ) WITHOUT ROWID",
        "CREATE INDEX document_trigrams_document_trigram ON document_trigrams (document, trigram)",
    ])
}
