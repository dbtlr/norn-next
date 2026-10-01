//! Where a link statement runs: the writer's connection inside a request or a
//! changeset, or a read snapshot.
//!
//! **One spelling of each statement, two places it runs.** The changeset's
//! re-decision reads the link index on the writer, inside its transaction;
//! the resolution change set ([`super::resolution`]) reads the same index on
//! a read snapshot, which a plan is resolved and checked against. Both read
//! the links one key holds, ask which keys anything is held under, and read
//! the head of what a key names, and each of those is one text in
//! [`super::statement`] whichever of the two runs it. So the plan seam's bars
//! over [`crate::ExplainedStatement::LinkHealthPathLinks`],
//! [`crate::ExplainedStatement::LinkHealthOccupied`] and
//! [`crate::ExplainedStatement::LinkHealthHeads`] bar what a snapshot runs
//! too: the text is the one they explain.
//!
//! **Each place counts what it runs as it always has.** The writer adds a
//! statement's steps to the request's [`ReadWork`]; a snapshot runs it through
//! [`Snapshot::run_statement`], the one place a snapshot's statements run, so
//! it is counted on [`Snapshot::counters`] and recorded under its
//! [`ResolutionStatement`] name.

use std::cell::RefCell;

use norn_db::rusqlite::types::Value;
use norn_db::rusqlite::{Connection, Row, params_from_iter};

use crate::error::{self, StoreError};
use crate::read::{Ran, ReadStatement};
use crate::request::{ReadWork, Reading, Request};
use crate::store::Snapshot;

/// A statement the resolution change set runs on a read snapshot, named so a
/// snapshot's record of what it ran says which it was. Each is a statement
/// the changeset's link-health re-decision runs too, spelled once.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolutionStatement {
    /// Which of a chunk of keys any link is held under: the path arm of
    /// [`crate::ExplainedStatement::LinkHealthOccupied`], an equality seek of
    /// the link index per key, each stopping at its first row.
    Occupied,
    /// A page of the links held under exactly one key, beside every key each
    /// is held under: [`crate::ExplainedStatement::LinkHealthPathLinks`].
    KeyLinks,
    /// The head of what each distinct key names, cut in the statement:
    /// [`crate::ExplainedStatement::LinkHealthHeads`].
    Heads,
}

impl From<ResolutionStatement> for ReadStatement {
    fn from(statement: ResolutionStatement) -> Self {
        ReadStatement::Resolution(statement)
    }
}

/// Where a link statement runs, and how what it costs is counted.
pub(crate) trait Runner {
    /// Run `sql`, bound to `values`, and read every row it answers through
    /// `read`; `statement` names it where the place records names, and
    /// `operation` says in words what it was doing where it fails.
    fn read_all<T>(
        &self,
        statement: ResolutionStatement,
        sql: String,
        values: Vec<Value>,
        read: impl FnMut(&Row<'_>) -> Reading<T>,
        operation: &'static str,
    ) -> Result<Vec<T>, StoreError>;
}

/// The writer's connection, inside a request or a changeset's transaction,
/// each statement's steps added to `work`.
pub(crate) struct OnWriter<'a> {
    pub(crate) connection: &'a Connection,
    pub(crate) work: &'a ReadWork,
}

impl Runner for OnWriter<'_> {
    fn read_all<T>(
        &self,
        _statement: ResolutionStatement,
        sql: String,
        values: Vec<Value>,
        read: impl FnMut(&Row<'_>) -> Reading<T>,
        operation: &'static str,
    ) -> Result<Vec<T>, StoreError> {
        Request::read_all_on(
            self.connection,
            self.work,
            &sql,
            params_from_iter(values),
            read,
            operation,
        )
    }
}

/// A read snapshot, each statement counted on it and recorded in `record` in
/// the order it ran.
pub(crate) struct OnSnapshot<'a> {
    pub(crate) snapshot: &'a Snapshot,
    pub(crate) record: RefCell<Vec<Ran>>,
}

impl<'a> OnSnapshot<'a> {
    /// Running on `snapshot`, nothing run yet.
    pub(crate) fn new(snapshot: &'a Snapshot) -> Self {
        OnSnapshot {
            snapshot,
            record: RefCell::new(Vec::new()),
        }
    }
}

impl Runner for OnSnapshot<'_> {
    fn read_all<T>(
        &self,
        statement: ResolutionStatement,
        sql: String,
        values: Vec<Value>,
        read: impl FnMut(&Row<'_>) -> Reading<T>,
        operation: &'static str,
    ) -> Result<Vec<T>, StoreError> {
        self.snapshot
            .run_statement(
                &mut self.record.borrow_mut(),
                Ran::new(statement, (sql, values)),
                read,
            )
            .map_err(|problem| error::sql(operation, problem))?
            .into_iter()
            .collect()
    }
}
