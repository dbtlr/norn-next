//! The plan handout: a statement, paired with the plan SQLite reported for it
//! and the columns SQLite reported it reads.
//!
//! A plan bar is worth something only against the SQL that actually ran, and a
//! crate that may not open a connection cannot take a plan of its own. So this
//! is the primitive a domain crate hands its own plans out through: it chooses
//! the statement and binds it as its execution site does, a caller judges the
//! plan, and the pair crosses the seam as plain data.
//!
//! **The columns are what a plan cannot say.** A plan names the relations a
//! statement reaches and the index it reaches each through, and never which
//! of a row's columns it reads — a probe that reads a row's widest column by
//! its row id plans exactly as one that reads only the id. SQLite's authorizer
//! reports each column read at preparation, so the reads are taken while the
//! explain is prepared, from the same text.
//!
//! **An explain is a report about a statement rather than a run of it.** What
//! taking one steps is not work a caller's bar attributes to its readers, so
//! nothing here touches a step count.

use std::collections::BTreeSet;

use rusqlite::{Params, Row};

use crate::database::Database;
use crate::error::{self, DbError};

/// One row of `EXPLAIN QUERY PLAN`, as SQLite reports it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanStep {
    pub id: i64,
    pub parent: i64,
    pub detail: String,
}

/// One column a statement reads: the table as the schema names it, never an
/// alias the statement gave it, and the column. SQLite reports a read that
/// names no column — a count of a table's rows — with the empty column.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ColumnRead {
    pub table: String,
    pub column: String,
}

/// The statement a reader emitted, paired with its plan and the columns it
/// reads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmittedPlan {
    pub sql: String,
    pub steps: Vec<PlanStep>,
    /// Every column the statement reads, each once.
    pub reads: BTreeSet<ColumnRead>,
}

impl Database {
    /// The plan SQLite reports for `sql` under the parameters its execution
    /// site binds, and the columns `sql` reads.
    ///
    /// The parameters are bound rather than left null because a plan is taken
    /// of a statement as it is executed: a statement whose text branches on
    /// what it is bound to would otherwise be explained in one state alone.
    /// The reads are recorded while the explain is prepared, which compiles
    /// `sql` whole, under the verdict of the connection's own authorizer.
    pub fn emitted_plan(&self, sql: &str, parameters: impl Params) -> Result<EmittedPlan, DbError> {
        let operation = "explaining an emitted statement";
        let explained = format!("EXPLAIN QUERY PLAN {sql}");
        let (mut statement, reads) = self.recording_reads(|connection| {
            connection
                .prepare(&explained)
                .map_err(|error| error::sql(operation, error))
        })?;
        let rows = statement
            .query_map(parameters, |row: &Row<'_>| {
                Ok(PlanStep {
                    id: row.get(0)?,
                    parent: row.get(1)?,
                    detail: row.get(3)?,
                })
            })
            .map_err(|error| error::sql(operation, error))?;
        let mut steps = Vec::new();
        for row in rows {
            steps.push(row.map_err(|error| error::sql(operation, error))?);
        }
        Ok(EmittedPlan {
            sql: sql.to_string(),
            steps,
            reads,
        })
    }
}
