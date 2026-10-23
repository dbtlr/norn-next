//! Disposable result storage, separate from durable database opens.

use rusqlite::trace::TraceEventCodes;
use rusqlite::{Connection, OpenFlags, Params, StatementStatus};
use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};

use crate::EmittedPlan;
use crate::database::{count_one_statement_begun, recording_reads};
use crate::error::{self, DbError};

/// One statement run while opening disposable storage, with its measured work.
#[derive(Clone, Debug)]
pub struct TemporarySetup {
    pub sql: String,
    pub full_scan_steps: u64,
    pub sorts: u64,
    pub vm_steps: u64,
}

/// An anonymous disk-backed database, removed by SQLite when it closes.
///
/// This is disposable scratch state, not a durable domain database. It has no
/// epoch or migration ceremony, and never attaches a source database.
pub struct TemporaryDatabase {
    connection: Connection,
    opening: Vec<TemporarySetup>,
    seal: Option<Arc<AtomicBool>>,
    file: Option<Arc<TemporaryFile>>,
}

/// Reopenable disposable storage. Each connection keeps this owner alive so
/// the final connection closes before the file is removed.
#[derive(Debug)]
pub struct TemporaryFile {
    path: PathBuf,
    #[cfg(unix)]
    identity: (u64, u64),
}

impl TemporaryFile {
    /// Open an independent sealed reader of the completed file.
    #[allow(clippy::disallowed_methods)] // Driver and disposable-file mechanics live here.
    pub fn open_reader(self: &Arc<Self>) -> Result<TemporaryDatabase, DbError> {
        self.verify_identity()?;
        let connection = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| error::sql("opening disposable results read-only", error))?;
        self.verify_identity()?;
        connection
            .pragma_update(None, "cache_size", -64)
            .map_err(|error| error::sql("bounding the result reader cache", error))?;
        connection
            .pragma_update(None, "query_only", true)
            .map_err(|error| error::sql("making result access read-only", error))?;
        let seal = Arc::new(AtomicBool::new(false));
        crate::database::arm_the_snapshot_control(&connection, Arc::clone(&seal))?;
        connection.trace_v2(
            TraceEventCodes::SQLITE_TRACE_STMT,
            Some(count_one_statement_begun),
        );
        Ok(TemporaryDatabase {
            connection,
            opening: Vec::new(),
            seal: Some(seal),
            file: Some(Arc::clone(self)),
        })
    }

    #[allow(clippy::disallowed_methods)] // File identity belongs to the substrate.
    fn verify_identity(&self) -> Result<(), DbError> {
        let metadata = fs::symlink_metadata(&self.path).map_err(|error| {
            file_error("checking disposable result identity", &self.path, error)
        })?;
        let same = metadata.is_file();
        #[cfg(unix)]
        let same = same && (metadata.dev(), metadata.ino()) == self.identity;
        if !same {
            return Err(DbError::Lifecycle {
                operation: "checking disposable result identity",
                path: self.path.clone(),
                message: "the result file was replaced".to_string(),
            });
        }
        Ok(())
    }
}

impl Drop for TemporaryFile {
    #[allow(clippy::disallowed_methods)] // Never remove a replacement at this name.
    fn drop(&mut self) {
        if self.verify_identity().is_ok() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn file_error(operation: &'static str, path: &Path, error: std::io::Error) -> DbError {
    DbError::Lifecycle {
        operation,
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

impl TemporaryDatabase {
    /// Allocate an exclusive name inside a configured disposable-storage directory.
    pub fn create_in(
        directory: &Path,
        cache_kib: u32,
        max_bytes: Option<u64>,
    ) -> Result<Self, DbError> {
        use std::hash::BuildHasher;
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let high = std::collections::hash_map::RandomState::new().hash_one(serial);
        let low = std::collections::hash_map::RandomState::new().hash_one(serial);
        Self::create(
            &directory.join(format!("result-{high:016x}{low:016x}.sqlite3")),
            cache_kib,
            max_bytes,
        )
    }
    /// Create private disposable storage without overwriting an existing name.
    #[allow(clippy::disallowed_methods)] // The substrate owns disposable files and opens.
    pub fn create(path: &Path, cache_kib: u32, max_bytes: Option<u64>) -> Result<Self, DbError> {
        if let Some(parent) = path.parent() {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            builder.mode(0o700);
            builder
                .create(parent)
                .map_err(|error| file_error("creating result directory", parent, error))?;
            if !fs::symlink_metadata(parent)
                .map_err(|error| file_error("checking result directory", parent, error))?
                .is_dir()
            {
                return Err(DbError::Lifecycle {
                    operation: "checking result directory",
                    path: parent.to_path_buf(),
                    message: "a result directory must not be a link".to_string(),
                });
            }
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let created = options
            .open(path)
            .map_err(|error| file_error("creating disposable results", path, error))?;
        #[cfg(unix)]
        let metadata = created
            .metadata()
            .map_err(|error| file_error("identifying disposable results", path, error))?;
        let file = Arc::new(TemporaryFile {
            path: path.to_path_buf(),
            #[cfg(unix)]
            identity: (metadata.dev(), metadata.ino()),
        });
        drop(created);
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| error::sql("opening disposable result storage", error))?;
        Self::configure(connection, Some(file), cache_kib, max_bytes)
    }

    /// The owner required to reopen this file; anonymous storage has none.
    pub fn shared_file(&self) -> Option<Arc<TemporaryFile>> {
        self.file.clone()
    }

    /// Open scratch storage with a bounded page cache and an optional allocated-page quota.
    #[allow(clippy::disallowed_methods)] // The substrate owns all connection opens.
    pub fn open(cache_kib: u32, max_bytes: Option<u64>) -> Result<Self, DbError> {
        let connection = Connection::open_with_flags(
            "",
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| error::sql("opening disposable result storage", error))?;
        Self::configure(connection, None, cache_kib, max_bytes)
    }

    fn configure(
        connection: Connection,
        file: Option<Arc<TemporaryFile>>,
        cache_kib: u32,
        max_bytes: Option<u64>,
    ) -> Result<Self, DbError> {
        let pages = max_bytes.map(|bytes| bytes / 4096);
        if pages.is_some_and(|pages| pages < 2) || cache_kib == 0 {
            return Err(DbError::Sql {
                operation: "opening disposable result storage",
                condition: None,
                message: "scratch storage needs a nonzero cache and at least two pages".to_string(),
            });
        }
        connection.trace_v2(
            TraceEventCodes::SQLITE_TRACE_STMT,
            Some(count_one_statement_begun),
        );
        let mut opening = Vec::new();
        let mut settings = vec![
            "PRAGMA page_size=4096".to_string(),
            format!("PRAGMA cache_size=-{cache_kib}"),
            "PRAGMA temp_store=FILE".to_string(),
            // Every result is built from empty and published only after success.
            // On failure it is closed, so it needs neither recovery nor a journal.
            "PRAGMA journal_mode=OFF".to_string(),
        ];
        if let Some(pages) = pages {
            settings.push(format!(
                "PRAGMA max_page_count={}",
                pages.min(i64::MAX as u64)
            ));
        }
        for sql in settings {
            let mut statement = connection
                .prepare(&sql)
                .map_err(|error| error::sql("preparing disposable result storage", error))?;
            {
                let mut rows = statement
                    .query([])
                    .map_err(|error| error::sql("configuring disposable result storage", error))?;
                while rows
                    .next()
                    .map_err(|error| error::sql("configuring disposable result storage", error))?
                    .is_some()
                {}
            }
            let count = |status| u64::from(statement.get_status(status) as u32);
            opening.push(TemporarySetup {
                sql,
                full_scan_steps: count(StatementStatus::FullscanStep),
                sorts: count(StatementStatus::Sort),
                vm_steps: count(StatementStatus::VmStep),
            });
        }
        Ok(Self {
            connection,
            opening,
            seal: None,
            file,
        })
    }

    /// The connection the result's owner composes its SQL against.
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Every setup statement that actually ran, in execution order.
    pub fn opening(&self) -> &[TemporarySetup] {
        &self.opening
    }

    /// Explain a statement on the disposable database that executes it.
    pub fn emitted_plan(&self, sql: &str, parameters: impl Params) -> Result<EmittedPlan, DbError> {
        crate::plan::explain(sql, parameters, |sql| {
            recording_reads(&self.connection, self.seal.clone(), |connection| {
                connection
                    .prepare(sql)
                    .map_err(|error| error::sql("explaining a result statement", error))
            })
        })
    }
}

/// Remove only this substrate's disposable-file names. The caller must hold
/// exclusive maintainership of the directory's store before sweeping.
#[allow(clippy::disallowed_methods)] // Scratch-file lifecycle is substrate-owned.
pub fn sweep_temporary_directory(directory: &Path) -> Result<usize, DbError> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(file_error("listing disposable results", directory, error)),
    };
    if !fs::symlink_metadata(directory)
        .map_err(|error| file_error("checking result directory", directory, error))?
        .is_dir()
    {
        return Err(DbError::Lifecycle {
            operation: "checking result directory",
            path: directory.to_path_buf(),
            message: "a result directory must not be a link".into(),
        });
    }
    let mut removed = 0;
    for entry in entries {
        let entry =
            entry.map_err(|error| file_error("listing disposable results", directory, error))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(token) = name
            .strip_prefix("result-")
            .and_then(|name| name.strip_suffix(".sqlite3"))
        else {
            continue;
        };
        if token.len() != 32 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        if entry
            .file_type()
            .map_err(|error| file_error("checking disposable result", &entry.path(), error))?
            .is_file()
        {
            fs::remove_file(entry.path())
                .map_err(|error| file_error("removing disposable results", &entry.path(), error))?;
            removed += 1;
        }
    }
    Ok(removed)
}
