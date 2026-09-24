//! Running a read's statements: each recorded as it ran, counted on the
//! snapshot, and explained from the record.

use std::collections::BTreeMap;

use norn_db::EmittedPlan;
use norn_db::rusqlite::types::Value;
use norn_db::rusqlite::{self, Row, StatementStatus, params_from_iter};

use super::{ReadFilter, ReadStatement};
use crate::error::{self, StoreError};
use crate::store::Snapshot;

/// One statement a read ran on its snapshot, as it ran: its name, the
/// filters it narrows by, its text and the values bound to it.
///
/// [`Snapshot::run_statement`] records one before it prepares the text the
/// record holds, which is how [`Snapshot::explained`] explains the statement
/// that ran rather than a second spelling of it.
pub(crate) struct Ran {
    pub(crate) statement: ReadStatement,
    pub(crate) filters: Vec<ReadFilter>,
    sql: String,
    values: Vec<Value>,
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
    fn of(statement: &rusqlite::Statement<'_>) -> Self {
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

impl Snapshot {
    /// Each statement in `record`, in the order it ran, as `plan` makes it of
    /// its name, its filters, and the plan SQLite reports for it: the text it
    /// ran, bound to the values it ran with, explained on this snapshot's
    /// read-only connection. An explain is a report about a statement rather
    /// than a run of it, so it is not counted.
    pub(crate) fn explained<P>(
        &self,
        record: Vec<Ran>,
        mut plan: impl FnMut(ReadStatement, Vec<ReadFilter>, EmittedPlan) -> P,
    ) -> Result<Vec<P>, StoreError> {
        record
            .into_iter()
            .map(|ran| {
                let emitted = norn_db::emitted_plan(
                    self.connection(),
                    &ran.sql,
                    params_from_iter(ran.values),
                )
                .map_err(StoreError::from)?;
                Ok(plan(ran.statement, ran.filters, emitted))
            })
            .collect()
    }

    /// Run `ran`, counted on this snapshot and recorded at the end of `record`
    /// before it is prepared, and read each row it answers through `read`.
    ///
    /// Every statement a read runs is run here, and what is prepared is the
    /// text the record holds, bound to the values it holds:
    /// [`Snapshot::explained`] explains the record, so the plan of a
    /// statement is the plan of what ran. Once every row is read, the record
    /// takes what SQLite counted stepping it ([`Stepped`]).
    pub(crate) fn run_statement<T>(
        &self,
        record: &mut Vec<Ran>,
        ran: Ran,
        read: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<Vec<T>> {
        self.run_statement_staged(record, ran, read)
            .map_err(StatementFailure::into_inner)
    }

    /// [`Snapshot::run_statement`], its failure saying whether the statement
    /// failed before it was stepped — preparing its text or binding its
    /// values — or while it was stepped.
    pub(super) fn run_statement_staged<T>(
        &self,
        record: &mut Vec<Ran>,
        ran: Ran,
        read: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>, StatementFailure> {
        self.count_statement();
        record.push(ran);
        let ran = record.last_mut().expect("the statement was just recorded");
        let mut statement = self
            .connection()
            .prepare(&ran.sql)
            .map_err(StatementFailure::Preparing)?;
        let rows = statement
            .query_map(params_from_iter(ran.values.iter()), read)
            .map_err(StatementFailure::Preparing)?
            .collect::<rusqlite::Result<Vec<T>>>()
            .map_err(StatementFailure::Stepping)?;
        ran.stepped = Stepped::of(&statement);
        Ok(rows)
    }

    /// Run one yes-or-no probe, which answers exactly one row.
    pub(super) fn ask(
        &self,
        record: &mut Vec<Ran>,
        probe: Ran,
        operation: &'static str,
    ) -> Result<bool, StoreError> {
        self.run_statement(record, probe, |row| row.get::<_, bool>(0))
            .and_then(|answers| {
                answers
                    .into_iter()
                    .next()
                    .ok_or(rusqlite::Error::QueryReturnedNoRows)
            })
            .map_err(|problem| error::sql(operation, problem))
    }
}

/// Where a statement a read ran failed.
pub(super) enum StatementFailure {
    /// Before it was stepped: its text did not prepare, or its values did not
    /// bind.
    Preparing(rusqlite::Error),
    /// While it was stepped, or while a row it answered was read.
    Stepping(rusqlite::Error),
}

impl StatementFailure {
    /// The driver's error, wherever it was met.
    fn into_inner(self) -> rusqlite::Error {
        match self {
            StatementFailure::Preparing(problem) | StatementFailure::Stepping(problem) => problem,
        }
    }
}
