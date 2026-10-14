//! `content_quarantines` — the bytes a content-quarantined place was judged over.
//!
//! A place whose bytes do not decode as a document holds no `documents` row,
//! only the content finding that says why
//! ([`norn_wire::FindingKind::BodyBytesNotUtf8`]). A row's `content_hash` is
//! what tells a reading of the same bytes that nothing changed, and this is
//! that hash for a place no row stands at: one row per place, written by the
//! act that files its content finding
//! ([`crate::DerivedFinding::bytes_hash`]) and read back by
//! [`crate::Request::content_quarantine_hash`].
//!
//! **A hash stands exactly while its content finding does.**
//! `content_quarantines_end_with_their_finding` deletes the place's row as a content
//! finding about it is deleted, by whichever act ends it — the place emptied, a
//! document derived there, a walk's prune, a schema pin's discard — so a hash
//! never says a place is unchanged where nothing stands to say what it is.
//! The trigger is the one maintainer, for the reason the tombstone clear is: a
//! second maintainer in code could disagree with it. Re-filing a place's
//! content finding discards the old finding before writing the new one, so
//! the trigger takes the old hash and the write records the new.
//!
//! `path` is the finding's subject, spelled as the finding spells it, and it is
//! the key: the read is one seek of the primary key.

use crate::write_path::WriteStatement;

pub(crate) fn statements() -> Vec<String> {
    let mut all = super::fixed(STATEMENTS);
    all.push(end_with_their_finding());
    all
}

const STATEMENTS: &[&str] = &["CREATE TABLE content_quarantines (
    path         TEXT NOT NULL PRIMARY KEY,
    content_hash TEXT NOT NULL
) WITHOUT ROWID"];

/// The trigger that ends a place's hash as a content finding about it goes,
/// its body [`WriteStatement::EndContentQuarantine`] with the deleted finding's
/// subject in place of the parameter, so the plan bar over that statement is a
/// plan of what the trigger runs.
fn end_with_their_finding() -> String {
    format!(
        "CREATE TRIGGER content_quarantines_end_with_their_finding AFTER DELETE ON findings
    WHEN old.kind = '{}'
BEGIN
    {};
END",
        crate::increment::CONTENT_FINDING_KIND.as_str(),
        WriteStatement::EndContentQuarantine
            .sql()
            .replace("?1", "old.path")
    )
}
