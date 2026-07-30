//! The store schema, stated once and designed whole.
//!
//! Every statement the store schema is made of lives in this module tree, one
//! area per file, and [`statements`] is the order they execute in. A database
//! is created by running exactly that list and by nothing else: there is no
//! incremental path from an older shape, because a store schema that is not
//! this one is resolved by discarding the database and running the list again.
//!
//! # The fingerprint is the whole list
//!
//! [`fingerprint`] digests every statement in order. A statement that is
//! reordered, reworded or reformatted changes it, which is deliberate: the
//! fingerprint answers *is the database in front of me the shape this build
//! writes*, and a whitespace edit that left it alone would be a shape this
//! build did not write. What it is not is a security primitive — it detects an
//! edit, not an adversary, so it is a 64-bit digest and says so.
//!
//! # Two kinds of table, and only one of them carries a schema key
//!
//! **Parse-fact tables are schema-independent.** `documents` and its derived
//! fact rows — links, headings, blocks, tags, the frontmatter projection —
//! record what a document *says*. The vault schema shapes none of it, so none
//! of them carries a vault-schema fingerprint and a schema edit re-derives
//! none of them.
//!
//! **Schema-dependent tables carry the vault-schema fingerprint they were
//! derived under**, so a schema edit re-derives exactly the tables it keys.
//! `findings` is that set today: whether vault state violates a rule is a
//! question the vault schema asks, so a finding derived under one schema says
//! nothing under another.
//!
//! # Foreign keys are the wholesale-replacement mechanism
//!
//! A document's derived fact rows exist only as long as the document does, so
//! every one of them carries `REFERENCES documents(id) ON DELETE CASCADE`. Two
//! operations rely on it: a hard delete removes the document row and the
//! cascade takes everything derived from it, and a re-derivation replaces one
//! document's fact rows wholesale. `PRAGMA foreign_keys` is off by default in
//! SQLite and is turned on per connection, which is why the store opens every
//! connection through one place.

pub(crate) mod documents;
pub(crate) mod facts;
pub(crate) mod findings;
pub(crate) mod fts;
pub(crate) mod meta;
pub(crate) mod migrations;
pub(crate) mod tombstones;
pub(crate) mod vectors;

/// The store schema version, pinned at 1 through the pre-release build.
///
/// It does not move: a DDL change during development is detected as a
/// [`fingerprint`] mismatch and resolved by rebuilding from zero, which
/// consumes no version numbers. At the first release version 1 freezes as the
/// first migratable baseline, and from then on the migrations pillar is the
/// evolution path.
pub const STORE_SCHEMA_VERSION: i64 = 1;

/// The areas, in execution order. `meta` is first because it is what an open
/// reads to decide whether the rest is trustworthy; `documents` precedes
/// everything that references it; the pillars come last because two of them
/// (full text, vectors) are defined over rows the earlier areas own.
const AREAS: &[&[&str]] = &[
    meta::STATEMENTS,
    documents::STATEMENTS,
    facts::STATEMENTS,
    tombstones::STATEMENTS,
    fts::STATEMENTS,
    vectors::STATEMENTS,
    findings::STATEMENTS,
    migrations::STATEMENTS,
];

/// Every statement the store schema is made of, in execution order.
pub fn statements() -> impl Iterator<Item = &'static str> {
    AREAS.iter().copied().flatten().copied()
}

/// The digest of the whole statement list, as sixteen lowercase hex digits.
///
/// FNV-1a over each statement in order, with a separator between them so that
/// moving a boundary between two statements changes the digest. The separator
/// is why `"CREATE TABLE a" + "(x)"` and `"CREATE TABLE a(x)"` do not collide.
///
/// **An edit-detector, not a security primitive.** Nothing here defends
/// against a chosen collision: the question it answers is whether a developer
/// changed the DDL under a database that already exists, and the cost of a
/// wrong answer is a rebuild that was not needed.
pub fn fingerprint() -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for statement in statements() {
        for byte in statement.as_bytes().iter().chain(b"\x1e") {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}
