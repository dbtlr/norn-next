//! Disposable result storage, separate from durable database opens.

use rusqlite::trace::TraceEventCodes;
use rusqlite::{Connection, OpenFlags, Params, StatementStatus};

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
}

impl TemporaryDatabase {
    /// Open scratch storage with a bounded page cache and an optional allocated-page quota.
    #[allow(clippy::disallowed_methods)] // The substrate owns all connection opens.
    pub fn open(cache_kib: u32, max_bytes: Option<u64>) -> Result<Self, DbError> {
        let pages = max_bytes.map(|bytes| bytes / 4096);
        if pages.is_some_and(|pages| pages < 2) || cache_kib == 0 {
            return Err(DbError::Sql {
                operation: "opening disposable result storage",
                condition: None,
                message: "scratch storage needs a nonzero cache and at least two pages".to_string(),
            });
        }
        let connection = Connection::open_with_flags(
            "",
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| error::sql("opening disposable result storage", error))?;
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
            recording_reads(&self.connection, None, |connection| {
                connection
                    .prepare(sql)
                    .map_err(|error| error::sql("explaining a result statement", error))
            })
        })
    }
}
