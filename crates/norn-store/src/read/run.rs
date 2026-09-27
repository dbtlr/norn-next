//! What a read runs its statements as, recorded so [`crate::store`] can run
//! and explain them: the counted run path is there, colocated with the
//! connection it runs on, so nothing else in this crate can reach that
//! connection around the counting.

use std::collections::BTreeMap;

use norn_db::rusqlite::types::Value;
use norn_db::rusqlite::{self, StatementStatus};

use super::{ReadFilter, ReadStatement};

/// One statement a read ran on its snapshot, as it ran: its name, the
/// filters it narrows by, its text and the values bound to it.
///
/// [`crate::store::Snapshot::run_statement`] records one before it prepares
/// the text the record holds, which is how
/// [`crate::store::Snapshot::explained`] explains the statement that ran
/// rather than a second spelling of it.
pub(crate) struct Ran {
    pub(crate) statement: ReadStatement,
    pub(crate) filters: Vec<ReadFilter>,
    pub(crate) sql: String,
    pub(crate) values: Vec<Value>,
    /// What SQLite counted while the statement was stepped, read once every
    /// row it answers has been read.
    pub(crate) stepped: Stepped,
}

/// What SQLite counts while one statement is stepped, read off the
/// statement's own status before it is dropped.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Stepped {
    /// Steps forward through a loop no constraint bounds: a table read end to
    /// end, or an index read end to end.
    pub(crate) full_scan_steps: u64,
    /// Sorts the statement ran: a temporary B-tree an `ORDER BY` filled
    /// because no index hands its rows back in order.
    pub(crate) sorts: u64,
    /// Virtual-machine operations the statement ran, whatever they did.
    pub(crate) vm_steps: u64,
}

impl Stepped {
    /// The counts `statement` holds, having been stepped to its end.
    pub(crate) fn of(statement: &rusqlite::Statement<'_>) -> Self {
        // SQLite keeps each count unsigned and hands it back as a C `int`, so
        // the bits read back as the unsigned count they are.
        let count = |status| u64::from(statement.get_status(status) as u32);
        Stepped {
            full_scan_steps: count(StatementStatus::FullscanStep),
            sorts: count(StatementStatus::Sort),
            vm_steps: count(StatementStatus::VmStep),
        }
    }

    /// Add what SQLite counted stepping another statement.
    pub(crate) fn add(&mut self, other: Stepped) {
        self.full_scan_steps += other.full_scan_steps;
        self.sorts += other.sorts;
        self.vm_steps += other.vm_steps;
    }
}

impl Ran {
    /// `statement`, composed as `(sql, values)`, narrowing by no filter.
    pub(crate) fn new(
        statement: impl Into<ReadStatement>,
        (sql, values): (String, Vec<Value>),
    ) -> Self {
        Ran {
            statement: statement.into(),
            filters: Vec::new(),
            sql,
            values,
            stepped: Stepped::default(),
        }
    }

    /// The same statement, narrowing by `filters`.
    pub(crate) fn narrowed_by(mut self, filters: Vec<ReadFilter>) -> Self {
        self.filters = filters;
        self
    }
}

/// What one request asked its snapshot, each question asked once — the active
/// fingerprint and which keys are known — and every statement the read ran,
/// in the order it ran them.
#[derive(Default)]
pub(crate) struct Lookups {
    /// The active fingerprint once read, `None` inside where no schema is
    /// pinned.
    pub(super) fingerprint: Option<Option<String>>,
    pub(super) known: BTreeMap<String, bool>,
    pub(crate) ran: Vec<Ran>,
}

/// Where a statement a read ran failed. [`crate::store::Snapshot::run_statement_staged`]
/// is the one place this is minted.
pub(crate) enum StatementFailure {
    /// Before it was stepped: its text did not prepare, or its values did not
    /// bind.
    Preparing(rusqlite::Error),
    /// While it was stepped, or while a row it answered was read.
    Stepping(rusqlite::Error),
}

impl StatementFailure {
    /// The driver's error, wherever it was met.
    pub(crate) fn into_inner(self) -> rusqlite::Error {
        match self {
            StatementFailure::Preparing(problem) | StatementFailure::Stepping(problem) => problem,
        }
    }
}
