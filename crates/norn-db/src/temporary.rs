//! Disposable result storage, separate from durable database opens.

use rusqlite::trace::TraceEventCodes;
use rusqlite::{Connection, OpenFlags, Params, StatementStatus};
#[allow(clippy::disallowed_types)] // norn-db owns disposable SQLite files, not vault handles.
use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, atomic::AtomicBool};

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

/// Disposable disk-backed storage, either anonymous or privately named.
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
    cleanup: TemporaryCleanupReceipt,
}

/// Final cleanup evidence independent of every file and connection owner.
#[derive(Clone, Debug)]
pub struct TemporaryCleanupReceipt {
    paths: Arc<[PathBuf]>,
    status: Arc<Mutex<TemporaryCleanupStatus>>,
}

/// Pending means a legitimate owner still exists; it is not a cleanup failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TemporaryCleanupStatus {
    Pending,
    Removed,
    Failed(DbError),
}

impl TemporaryCleanupReceipt {
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    pub fn status(&self) -> TemporaryCleanupStatus {
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// Loss of disposable storage is distinct from a failed attempt to inspect or read it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TemporaryReadError {
    Lost { path: PathBuf },
    Failed(DbError),
}

impl From<DbError> for TemporaryReadError {
    fn from(error: DbError) -> Self {
        Self::Failed(error)
    }
}

impl std::fmt::Display for TemporaryReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lost { path } => write!(
                formatter,
                "disposable storage at {} is missing or replaced",
                path.display()
            ),
            Self::Failed(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for TemporaryReadError {}

/// Non-owning evidence: successful drop must close owners and remove actual paths.
#[cfg(feature = "induced-failure")]
#[derive(Debug)]
pub struct RetirementCheck {
    owner: Option<std::sync::Weak<TemporaryFile>>,
    paths: Vec<PathBuf>,
}

#[cfg(feature = "induced-failure")]
impl RetirementCheck {
    pub(crate) fn database(path: &Path) -> Self {
        let paths = crate::database::database_paths(path).into();
        Self { owner: None, paths }
    }

    /// This observes cleanup; it never deletes residue or closes a live owner.
    #[allow(clippy::disallowed_methods)] // File lifecycle evidence belongs to the substrate.
    pub fn verify(&self) -> Result<&[PathBuf], DbError> {
        if self
            .owner
            .as_ref()
            .is_some_and(|owner| owner.strong_count() != 0)
        {
            return Err(DbError::Lifecycle {
                operation: "verifying disposable retirement",
                path: self.paths[0].clone(),
                message: "a disposable file still has an owner".into(),
            });
        }
        for path in &self.paths {
            match fs::symlink_metadata(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => {
                    return Err(file_error("verifying disposable retirement", path, error));
                }
                Ok(_) => {
                    return Err(DbError::Lifecycle {
                        operation: "verifying disposable retirement",
                        path: path.clone(),
                        message: "disposable storage remains after retirement".into(),
                    });
                }
            }
        }
        Ok(&self.paths)
    }
}

impl TemporaryFile {
    /// Retain the eventual cleanup outcome without retaining this file.
    pub fn cleanup_receipt(&self) -> TemporaryCleanupReceipt {
        self.cleanup.clone()
    }

    #[cfg(feature = "induced-failure")]
    pub fn retirement_check(self: &Arc<Self>) -> RetirementCheck {
        let mut check = RetirementCheck::database(&self.path);
        check.owner = Some(Arc::downgrade(self));
        check
    }

    /// Immutable source reads retain the ordinary snapshot seal.
    pub fn open_snapshot(self: &Arc<Self>) -> Result<crate::Database, TemporaryReadError> {
        let (database, _owner) = self.open_reader()?.into_database()?;
        Ok(database)
    }
    /// Open an independent sealed reader of the completed file.
    #[allow(clippy::disallowed_methods)] // Driver and disposable-file mechanics live here.
    pub fn open_reader(self: &Arc<Self>) -> Result<TemporaryDatabase, TemporaryReadError> {
        self.verify_identity()?;
        let connection = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| error::sql("opening disposable results read-only", error))?;
        self.verify_identity()?;
        // Published results have no writer. Contention is external and retryable,
        // so it must not start another busy-wait interval after source acquisition.
        connection
            .busy_timeout(std::time::Duration::ZERO)
            .map_err(|error| error::sql("refusing result reader contention", error))?;
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
    fn verify_identity(&self) -> Result<(), TemporaryReadError> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(TemporaryReadError::Lost {
                    path: self.path.clone(),
                });
            }
            Err(error) => {
                return Err(
                    file_error("checking disposable result identity", &self.path, error).into(),
                );
            }
        };
        let same = metadata.is_file();
        #[cfg(unix)]
        let same = same && (metadata.dev(), metadata.ino()) == self.identity;
        if !same {
            return Err(TemporaryReadError::Lost {
                path: self.path.clone(),
            });
        }
        Ok(())
    }
}

impl Drop for TemporaryFile {
    #[allow(clippy::disallowed_methods)] // Never remove a replacement at this name.
    fn drop(&mut self) {
        let result = match self.verify_identity() {
            Ok(()) => {
                // No writer survives the final owner. Remove its necessary sidecars
                // before the base name; a replaced base must protect all names.
                self.cleanup
                    .paths
                    .iter()
                    .skip(1)
                    .chain(self.cleanup.paths.iter().take(1))
                    .try_for_each(|path| match fs::remove_file(path) {
                        Ok(()) => Ok(()),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                        Err(error) => Err(file_error("removing disposable storage", path, error)),
                    })
            }
            Err(TemporaryReadError::Lost { .. }) => {
                match fs::symlink_metadata(&self.path) {
                    // A new maintainer may have swept a retired file. Held handles
                    // finish independently. All necessary names must be absent.
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        self.cleanup.paths.iter().skip(1).try_for_each(|path| {
                            match fs::symlink_metadata(path) {
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                            Err(error) => {
                                Err(file_error("checking disposable cleanup", path, error))
                            }
                            Ok(_) => Err(DbError::Lifecycle {
                                operation: "checking disposable cleanup",
                                path: path.clone(),
                                message:
                                    "a sidecar remains after the disposable main file was removed"
                                        .into(),
                            }),
                        }
                        })
                    }
                    Err(error) => Err(file_error("checking disposable cleanup", &self.path, error)),
                    Ok(_) => Err(DbError::Lifecycle {
                        operation: "removing disposable storage",
                        path: self.path.clone(),
                        message: "the disposable file was replaced; replacement retained".into(),
                    }),
                }
            }
            Err(TemporaryReadError::Failed(error)) => Err(error),
        };
        *self
            .cleanup
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = match result {
            Ok(()) => TemporaryCleanupStatus::Removed,
            Err(error) => TemporaryCleanupStatus::Failed(error),
        };
    }
}

fn file_error(operation: &'static str, path: &Path, error: std::io::Error) -> DbError {
    DbError::Lifecycle {
        operation,
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

/// List candidate storage parents without opening a source or inspecting child storage.
#[allow(clippy::disallowed_methods)] // Derived SQLite-file lifecycle belongs to this substrate.
pub fn temporary_store_parents(root: &Path) -> Result<Vec<PathBuf>, DbError> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(file_error("discovering disposable storage", root, error)),
    };
    let mut parents = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| file_error("listing derived stores", root, error))?;
        if !entry
            .file_type()
            .map_err(|error| file_error("checking derived store", &entry.path(), error))?
            .is_dir()
        {
            continue;
        }
        parents.push(entry.path());
    }
    parents.sort();
    Ok(parents)
}

impl TemporaryDatabase {
    /// Hand a disposable domain writer to the ordinary database machinery.
    /// The database retains the file until its connection closes; the returned
    /// owner also permits reopening independently of that connection's lifetime.
    pub fn into_database(self) -> Result<(crate::Database, Arc<TemporaryFile>), DbError> {
        let file = self.file.ok_or_else(|| DbError::Sql {
            operation: "adopting disposable domain storage",
            condition: None,
            message: "a reopenable source needs named scratch storage".into(),
        })?;
        let mut database = crate::Database::adopt(self.connection, &file.path)?;
        database.temporary_file = Some(Arc::clone(&file));
        Ok((database, file))
    }
    /// Allocate an exclusive name inside a configured disposable-storage directory.
    pub fn create_in(
        directory: &Path,
        cache_kib: u32,
        max_bytes: Option<u64>,
    ) -> Result<Self, DbError> {
        Self::create_in_observed(directory, cache_kib, max_bytes, |_| {})
    }

    /// Publish non-owning cleanup evidence as soon as exclusive creation succeeds,
    /// including when later connection setup fails. The operation error is unchanged.
    pub fn create_in_observed(
        directory: &Path,
        cache_kib: u32,
        max_bytes: Option<u64>,
        observe: impl FnOnce(&TemporaryCleanupReceipt),
    ) -> Result<Self, DbError> {
        use std::hash::BuildHasher;
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let high = std::collections::hash_map::RandomState::new().hash_one(serial);
        let low = std::collections::hash_map::RandomState::new().hash_one(serial);
        Self::create_observed(
            &directory.join(format!("result-{high:016x}{low:016x}.sqlite3")),
            cache_kib,
            max_bytes,
            observe,
        )
    }
    /// Create private disposable storage without overwriting an existing name.
    #[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The substrate owns disposable SQLite files and opens.
    pub fn create(path: &Path, cache_kib: u32, max_bytes: Option<u64>) -> Result<Self, DbError> {
        Self::create_observed(path, cache_kib, max_bytes, |_| {})
    }

    #[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Exclusive disposable-file creation is substrate-owned.
    fn create_observed(
        path: &Path,
        cache_kib: u32,
        max_bytes: Option<u64>,
        observe: impl FnOnce(&TemporaryCleanupReceipt),
    ) -> Result<Self, DbError> {
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
            cleanup: TemporaryCleanupReceipt {
                paths: crate::database::database_paths(path).into(),
                status: Arc::new(Mutex::new(TemporaryCleanupStatus::Pending)),
            },
        });
        drop(created);
        observe(&file.cleanup);
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
        let base = ["-wal", "-shm", "-journal"]
            .iter()
            .find_map(|suffix| name.strip_suffix(suffix))
            .unwrap_or(name);
        let Some(token) = base
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
