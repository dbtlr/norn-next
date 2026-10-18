//! The migrations pillar — the store schema's evolution path, carved and empty.
//!
//! `migrations` is the ledger of applied store schema migrations: which version
//! was applied, at which write generation, when, and the DDL fingerprint the
//! database carried afterwards.
//!
//! # It is empty through the pre-release build, and that is the design
//!
//! Before release, a DDL fingerprint or store-schema version mismatch rebuilds
//! the derived database from zero. Nothing migrates, so there is no ledger
//! row to record; `meta` already records the current schema and fingerprint.
//!
//! The first release freezes a migratable baseline. Its baseline row arrives
//! from the code that freezes it, and later schema migrations record rows here.
//!
//! The table exists now because its absence would make that first release a
//! schema change to a table nobody can migrate — and because the carve is what
//! keeps rebuild-from-zero honest about its lifetime: after the freeze, rung 3
//! narrows to damaged state, and evolution belongs here.

pub(crate) fn statements() -> Vec<String> {
    super::fixed(STATEMENTS)
}

const STATEMENTS: &[&str] = &["CREATE TABLE migrations (
    version            INTEGER PRIMARY KEY,
    ddl_fingerprint    TEXT    NOT NULL,
    applied_generation INTEGER NOT NULL,
    applied_at         INTEGER NOT NULL
) WITHOUT ROWID"];
