//! Name-only vault files, separate from every document projection.
//!
//! Files keep their extension in the suffix key. A `photo.png` file and a
//! `photo.png.md` document therefore share a class. The resolver unions their
//! indexed reads; document queries read their own table. Negative file ids
//! keep its row identities disjoint from document ids, and let both arms seek
//! their primary keys without a computed discriminator. Anchors are judged
//! only for the positive, document identities.

pub(crate) fn statements() -> Vec<String> {
    super::fixed(&[
        "CREATE TABLE vault_files (
            id INTEGER PRIMARY KEY CHECK (id < 0),
            path TEXT NOT NULL CHECK (path <> ''),
            suffix_key TEXT NOT NULL,
            path_key TEXT NOT NULL,
            admitting_segments INTEGER NOT NULL CHECK (admitting_segments >= 1),
            generation INTEGER NOT NULL
        )",
        "CREATE UNIQUE INDEX vault_files_path ON vault_files(path)",
        "CREATE INDEX vault_files_path_nocase ON vault_files(path COLLATE NOCASE, path)",
        "CREATE INDEX vault_files_path_key ON vault_files(path_key, path)",
        "CREATE INDEX vault_files_suffix_key ON vault_files(suffix_key)",
        "CREATE INDEX vault_files_admitted_suffix_key
            ON vault_files(admitting_segments, suffix_key, path)",
    ])
}
