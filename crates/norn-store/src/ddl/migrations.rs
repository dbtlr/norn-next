//! The migrations pillar — the store schema's evolution path, carved and empty.
//!
//! `migrations` is the ledger of applied store schema migrations: which version
//! was applied, at which write generation, when, and the DDL fingerprint the
//! database carried afterwards.
//!
//! # It is empty through the pre-release build, and that is the design
//!
//! Store schema version is pinned at 1 and does not move: a DDL change during
//! development is a fingerprint mismatch resolved by rebuilding from zero,
//! which consumes no version numbers. Nothing is migrating, so there is nothing
//! to record, and recording version 1 here at create time would put the same
//! fact in two places — `meta` already answers *what shape is this database*,
//! and this table answers *how did it get here*.
//!
//! At the first release version 1 freezes as the first migratable baseline. The
//! baseline row arrives then, from the code that freezes it, and from that point
//! every store schema change is a migration with a row here rather than a
//! rebuild.
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
