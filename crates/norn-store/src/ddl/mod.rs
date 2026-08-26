//! The store schema, stated once and designed whole.
//!
//! Every statement the store schema is made of lives in this module tree, one
//! area per file — with one exception, `meta`, whose table is the substrate's
//! and whose statement this list takes from `norn-db` — and [`statements`] is
//! the order they execute in. A database is created by running exactly that
//! list and by nothing else: there is no incremental path from an older shape,
//! because a store schema that is not this one is resolved by discarding the
//! database and running the list again.
//!
//! # The fingerprint is the whole list, and it is only half the question
//!
//! [`fingerprint`] digests every statement in order. A statement that is
//! reordered, reworded or reformatted changes it, which is deliberate: the
//! fingerprint answers *which statement list produced this database*, and a
//! whitespace edit that left it alone would be a shape this build did not write.
//! What it is not is a security primitive — it detects an edit, not an
//! adversary, so it is a 64-bit digest and says so.
//!
//! What the fingerprint cannot answer is whether the database still *holds* what
//! that list created: it is compared against a value the database reports about
//! itself, so a dropped table, index or trigger leaves it intact. That question
//! is the schema digest's, taken over `sqlite_schema` at create and re-taken at
//! every open — see [`crate::Store`].
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
//! A **parse-fact** row exists only as long as the document it was read from, so
//! every one of them carries `REFERENCES documents(id) ON DELETE CASCADE`. Two
//! operations rely on it: a hard delete removes the document
//! row and the cascade takes everything derived from it, and a re-derivation
//! replaces one document's fact rows wholesale. `PRAGMA foreign_keys` is off by
//! default in SQLite and is turned on per connection, which is why the store
//! opens every connection through one place.
//!
//! `findings` is deliberately outside that: a finding is keyed by path and class
//! and outlives its subject, so no document delete reaches it and its lifecycle is
//! class-scoped maintenance. The `findings` area in this tree states the whole of
//! it beside its own statements.

pub(crate) mod documents;
pub(crate) mod facts;
pub(crate) mod findings;
pub(crate) mod fts;
pub(crate) mod meta;
pub(crate) mod migrations;
pub(crate) mod tombstones;

/// The store schema version, pinned at 1 through the pre-release build.
///
/// It does not move: a DDL change during development is detected as a
/// [`fingerprint`] mismatch and resolved by rebuilding from zero, which
/// consumes no version numbers. At the first release version 1 freezes as the
/// first migratable baseline, and from then on the migrations pillar is the
/// evolution path.
pub const STORE_SCHEMA_VERSION: i64 = 1;

/// The areas, in execution order. `meta` is first, and it is the substrate's
/// table rather than this tree's, because it is what an open reads to decide
/// whether the rest is trustworthy; `documents` precedes everything that
/// references it; the pillars come last because one of them (full text) is
/// defined over rows the earlier areas own.
///
/// An area is a function rather than a slice of literals because a statement
/// may carry a bound the Rust API also states — `finding_candidates`'s rank
/// `CHECK` is [`crate::CANDIDATE_HEAD`] — and the statement builds that bound
/// from the constant instead of spelling it a second time.
const AREAS: &[fn() -> Vec<String>] = &[
    norn_db::meta::statements,
    documents::statements,
    facts::statements,
    tombstones::statements,
    fts::statements,
    findings::statements,
    migrations::statements,
];

/// Every statement the store schema is made of, in execution order.
pub fn statements() -> Vec<String> {
    AREAS.iter().flat_map(|area| area()).collect()
}

/// The statements an area states literally, as the list the areas return.
pub(crate) fn fixed(statements: &[&str]) -> Vec<String> {
    statements
        .iter()
        .map(|statement| (*statement).to_string())
        .collect()
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
/// wrong answer is a rebuild that was not needed. Its failure directions are
/// not symmetric: a digest that changed when the shape did not costs one
/// unnecessary rebuild of rebuildable state, and a collision — two shapes
/// digesting the same — leaves a database this build did not write in place,
/// which is what the schema digest folded into an open ([`crate::Store`])
/// catches independently.
pub fn fingerprint() -> String {
    norn_db::digest(statements().iter().map(String::as_str))
}
