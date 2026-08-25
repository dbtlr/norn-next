//! The tolerant reader every machine-local file is read through.
//!
//! Machine-local files outlive the binary that wrote them, and two binaries of
//! different ages read the same file on the same machine — a released host and
//! a development client, an old service unit and a new CLI. Three behaviours
//! make that survivable, and they are all here rather than in each file's own
//! module:
//!
//! - **A version ahead of this build is refused.** Reading it would mean
//!   dropping the fields this build does not know, and the next write would
//!   make the drop permanent.
//! - **A version behind this build is migrated in memory**, and the file is
//!   left alone. A read never rewrites: a plain `norn list` on a machine that
//!   also runs an older service must not change the bytes under it. The
//!   migrated form reaches the disk on the next write that was going to
//!   happen anyway.
//! - **A field this build does not know is kept.** The document is held as the
//!   untyped table it was parsed from, the typed view is projected out of it,
//!   and a write edits the keys the typed view owns and leaves every other key
//!   exactly where it was. This is why the reader is not a derived
//!   deserializer: a struct with named fields cannot hold what it has no field
//!   for. **Keys are what is preserved, not text**: a write re-renders the
//!   document, so comments and key order are the writer's rather than the
//!   file's.
//!
//! # What is refused, and how it is repaired
//!
//! A file this reader cannot make sense of is refused whole — a zero-byte file
//! carries no `version` and is corrupt rather than empty, and one malformed
//! entry refuses the file it is in rather than being dropped. Nothing here
//! repairs a file: a mutation reads before it writes, so it refuses on the same
//! terms, and the recovery is a person editing the file (or removing it, which
//! reads as a first run). Guessing at what a broken entry meant is how a
//! reader turns a file somebody can still fix into one nobody can.
//!
//! Only version 1 exists. What is built here is the seam, not a migration
//! ladder: [`migrate`] is the one place a version-behind reading is handled,
//! and today it has nothing to handle.

use std::path::Path;

use toml::Table;
use toml::Value;

use crate::error::{ConfigError, corrupt};

/// The schema version this build reads and writes.
pub(crate) const SCHEMA_VERSION: i64 = 1;

/// The key carrying the schema version. Every machine-local file has one, and
/// a file without it is corrupt rather than assumed current — an assumed
/// version is how a reader silently mangles a format it has never seen.
const VERSION_KEY: &str = "version";

/// One versioned machine-local file, as read.
///
/// Holds the whole document, unknown keys included, so that a rewrite is an
/// edit of the keys the typed view owns rather than a fresh serialization of
/// what this build happens to model.
#[derive(Clone, Debug)]
pub(crate) struct Document {
    table: Table,
}

impl Document {
    /// The document a file that is not there stands for: current version, no
    /// content. A first run and an empty registry are the same reading, which
    /// is what keeps a missing file from being an error every caller handles.
    pub(crate) fn empty() -> Self {
        let mut table = Table::new();
        table.insert(VERSION_KEY.to_string(), Value::Integer(SCHEMA_VERSION));
        Document { table }
    }

    /// Read `bytes` as a versioned document, or say what is wrong with them.
    pub(crate) fn parse(path: &Path, bytes: &[u8]) -> Result<Self, ConfigError> {
        let text = std::str::from_utf8(bytes)
            .map_err(|error| corrupt(path, format!("the file is not UTF-8: {error}")))?;
        let table: Table = text
            .parse()
            .map_err(|error| corrupt(path, format!("the file is not TOML: {error}")))?;

        let found = match table.get(VERSION_KEY) {
            Some(Value::Integer(found)) => *found,
            Some(other) => {
                return Err(corrupt(
                    path,
                    format!(
                        "`{VERSION_KEY}` is {}, and a schema version is an integer",
                        other.type_str()
                    ),
                ));
            }
            None => {
                return Err(corrupt(
                    path,
                    format!("the file carries no `{VERSION_KEY}`, so its schema is unknown"),
                ));
            }
        };

        if found > SCHEMA_VERSION {
            return Err(ConfigError::VersionAhead {
                path: path.to_path_buf(),
                found,
                supported: SCHEMA_VERSION,
            });
        }
        let table = if found < SCHEMA_VERSION {
            migrate(path, table, found)?
        } else {
            table
        };
        Ok(Document { table })
    }

    /// The document at `path`, or the empty one when nothing is there.
    pub(crate) fn read(path: &Path, bytes: Option<&[u8]>) -> Result<Self, ConfigError> {
        match bytes {
            Some(bytes) => Document::parse(path, bytes),
            None => Ok(Document::empty()),
        }
    }

    /// The sub-table under `key`, or the empty one when the document carries
    /// none.
    pub(crate) fn section(&self, path: &Path, key: &str) -> Result<Table, ConfigError> {
        match self.table.get(key) {
            Some(Value::Table(table)) => Ok(table.clone()),
            Some(other) => Err(corrupt(
                path,
                format!("`{key}` is {}, and it holds a table", other.type_str()),
            )),
            None => Ok(Table::new()),
        }
    }

    /// Put `section` back under `key`, replacing whatever was there.
    ///
    /// The caller builds `section` out of the sub-tables it read, so a key
    /// inside an entry that this build does not model survives the round trip
    /// as long as the caller carried it — which is what the entry projections
    /// do.
    pub(crate) fn set_section(&mut self, key: &str, section: Table) {
        self.table.insert(key.to_string(), Value::Table(section));
    }

    /// The document as TOML, stamped with the version this build writes.
    ///
    /// Stamping here is what makes an in-memory migration durable exactly once
    /// and only on a write: a document read from an older version carries the
    /// older number until something legitimately rewrites the file. The stamp
    /// lands in the document rather than in a copy of it, so rendering a
    /// registry does not duplicate the registry.
    pub(crate) fn render(&mut self, path: &Path) -> Result<String, ConfigError> {
        self.table
            .insert(VERSION_KEY.to_string(), Value::Integer(SCHEMA_VERSION));
        toml::to_string_pretty(&self.table).map_err(|error| {
            corrupt(
                path,
                format!("the document is not writable as TOML: {error}"),
            )
        })
    }
}

/// Bring a document written by an older build up to [`SCHEMA_VERSION`], in
/// memory.
///
/// The seam exists before the first migration does. Version 1 is the first
/// version there has ever been, so every number below it names a file no
/// release ever wrote, and the honest answer for one is that it is not a
/// document this format has: a guess at what a version-zero file meant would
/// be a guess at a format that never existed.
fn migrate(path: &Path, _table: Table, found: i64) -> Result<Table, ConfigError> {
    Err(corrupt(
        path,
        format!("schema version {found} is below version {SCHEMA_VERSION}, which no build wrote"),
    ))
}

/// The string under `key`, or `None` when the entry does not carry it.
pub(crate) fn optional_string(
    path: &Path,
    context: &str,
    entry: &Table,
    key: &str,
) -> Result<Option<String>, ConfigError> {
    match entry.get(key) {
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(other) => Err(corrupt(
            path,
            format!(
                "{context}: `{key}` is {}, and it holds a string",
                other.type_str()
            ),
        )),
        None => Ok(None),
    }
}

/// The string under `key`, or a refusal naming the entry that omitted it.
pub(crate) fn required_string(
    path: &Path,
    context: &str,
    entry: &Table,
    key: &str,
) -> Result<String, ConfigError> {
    optional_string(path, context, entry, key)?
        .ok_or_else(|| corrupt(path, format!("{context}: `{key}` is required")))
}

/// The integer under `key`, or a refusal naming the entry that omitted it.
pub(crate) fn required_integer(
    path: &Path,
    context: &str,
    entry: &Table,
    key: &str,
) -> Result<i64, ConfigError> {
    match entry.get(key) {
        Some(Value::Integer(value)) => Ok(*value),
        Some(other) => Err(corrupt(
            path,
            format!(
                "{context}: `{key}` is {}, and it holds an integer",
                other.type_str()
            ),
        )),
        None => Err(corrupt(path, format!("{context}: `{key}` is required"))),
    }
}

/// The sub-table under `key` of a section, or a refusal saying what is there
/// instead.
pub(crate) fn entry_table(path: &Path, context: &str, value: &Value) -> Result<Table, ConfigError> {
    match value {
        Value::Table(table) => Ok(table.clone()),
        other => Err(corrupt(
            path,
            format!("{context} is {}, and an entry is a table", other.type_str()),
        )),
    }
}

/// Set `key` to `value`, or remove it when there is no value.
///
/// Removing rather than writing an empty marker is what keeps an absent
/// optional field absent: the file says nothing about a field that means
/// nothing, and the reader's default is the one place the meaning of absence
/// lives.
pub(crate) fn set_optional(entry: &mut Table, key: &str, value: Option<Value>) {
    match value {
        Some(value) => {
            entry.insert(key.to_string(), value);
        }
        None => {
            entry.remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at() -> &'static Path {
        Path::new("/config/norn/registry.toml")
    }

    #[test]
    fn a_file_that_is_not_there_reads_as_the_current_version() {
        let mut document = Document::read(at(), None).expect("the empty document");
        let rendered = document.render(at()).expect("rendering");
        assert!(rendered.contains("version = 1"), "{rendered}");
    }

    /// A document holding both a value and a table round-trips: TOML requires
    /// every top-level value ahead of every top-level table, and the version
    /// key does not sort ahead of the sections it accompanies.
    #[test]
    fn a_rendered_document_puts_its_version_ahead_of_its_sections() {
        let mut document = Document::empty();
        let mut section = Table::new();
        section.insert("one".to_string(), Value::Table(Table::new()));
        document.set_section("vaults", section);

        let rendered = document.render(at()).expect("rendering");
        let reread = Document::parse(at(), rendered.as_bytes()).expect("re-reading");
        assert!(
            reread
                .section(at(), "vaults")
                .expect("a section")
                .contains_key("one")
        );
    }

    #[test]
    fn a_version_ahead_of_this_build_is_refused() {
        let error = Document::parse(at(), b"version = 2\n").expect_err("a newer file");
        assert_eq!(
            error,
            ConfigError::VersionAhead {
                path: at().to_path_buf(),
                found: 2,
                supported: 1,
            }
        );
    }

    #[test]
    fn a_file_with_no_version_is_corrupt() {
        let error = Document::parse(at(), b"vaults = {}\n").expect_err("an unversioned file");
        assert!(matches!(error, ConfigError::Corrupt { .. }), "{error}");
    }

    #[test]
    fn a_version_below_the_first_one_is_corrupt() {
        let error = Document::parse(at(), b"version = 0\n").expect_err("a version nobody wrote");
        let ConfigError::Corrupt { reason, .. } = error else {
            panic!("expected a corrupt reading");
        };
        assert!(reason.contains("version 0"), "{reason}");
    }

    #[test]
    fn bytes_that_are_not_toml_are_corrupt_with_the_reason() {
        let error = Document::parse(at(), b"= not toml\n").expect_err("bytes that are not TOML");
        let ConfigError::Corrupt { reason, .. } = error else {
            panic!("expected a corrupt reading");
        };
        assert!(reason.contains("not TOML"), "{reason}");
    }
}
