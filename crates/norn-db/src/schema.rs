//! Two digests, and the question each of them answers.
//!
//! [`digest`] over a statement list is a **DDL fingerprint**, and it answers
//! *which statement list produced this database*. A statement that is
//! reordered, reworded or reformatted changes it, which is deliberate: a
//! whitespace edit that left it alone would be a shape this build did not
//! write.
//!
//! What a fingerprint cannot answer is whether the database still *holds* what
//! that list created: it is compared against a value the database reports
//! about itself, so a dropped table, index or trigger leaves it intact. That
//! question is [`schema_digest`]'s, taken over `sqlite_schema` at create and
//! re-taken at every open.
//!
//! **Both are edit-detectors, not security primitives.** Nothing here defends
//! against a chosen collision: the question they answer is whether a shape
//! changed under a database that already exists, and the cost of a wrong
//! answer is a rebuild that was not needed. The failure directions are not
//! symmetric — a digest that changed when the shape did not costs one
//! unnecessary rebuild of rebuildable state, and a collision leaves a database
//! this build did not write in place, which is what the second digest catches
//! independently of the first.

use rusqlite::Connection;

/// FNV-1a over a sequence of parts, as sixteen lowercase hex digits.
///
/// Each part is followed by a separator, so that moving a boundary between two
/// parts changes the digest rather than leaving the concatenation unchanged.
/// The separator is why `"CREATE TABLE a" + "(x)"` and `"CREATE TABLE a(x)"`
/// do not collide.
pub fn digest<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in part.as_bytes().iter().chain(b"\x1e") {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

/// The digest of the schema a database actually holds.
///
/// Every object, in a fixed order, by kind, name and the statement that
/// created it. A dropped index, table or trigger changes it; so does one
/// redefined behind the owner's back. `sql` is null for an index SQLite
/// created for a constraint, and the empty string stands in for it.
pub fn schema_digest(connection: &Connection) -> rusqlite::Result<String> {
    let mut statement = connection
        .prepare("SELECT type, name, ifnull(sql, '') FROM sqlite_schema ORDER BY 1, 2")?;
    let mut parts: Vec<String> = Vec::new();
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        parts.push(row.get(0)?);
        parts.push(row.get(1)?);
        parts.push(row.get(2)?);
    }
    Ok(digest(parts.iter().map(String::as_str)))
}
