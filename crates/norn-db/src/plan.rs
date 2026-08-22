//! The plan handout: a statement, paired with the plan SQLite reported for it.
//!
//! A plan bar is worth something only against the SQL that actually ran, and a
//! crate that may not open a connection cannot take a plan of its own. So this
//! is the primitive a domain crate hands its own plans out through: it chooses
//! the statement and binds it as its execution site does, a caller judges the
//! plan, and the pair crosses the seam as plain data.
//!
//! **An explain is a report about a statement rather than a run of it.** What
//! taking one steps is not work a caller's bar attributes to its readers, so
//! nothing here touches a step count.

use rusqlite::{Connection, Params, Row};

use crate::error::{self, DbError};

/// One row of `EXPLAIN QUERY PLAN`, as SQLite reports it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanStep {
    pub id: i64,
    pub parent: i64,
    pub detail: String,
}

/// The statement a reader emitted, paired with its plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmittedPlan {
    pub sql: String,
    pub steps: Vec<PlanStep>,
}

/// The plan SQLite reports for `sql` under the parameters its execution site
/// binds.
///
/// The parameters are bound rather than left null because a plan is taken of a
/// statement as it is executed: a statement whose text branches on what it is
/// bound to would otherwise be explained in one state alone.
pub fn emitted_plan(
    connection: &Connection,
    sql: &str,
    parameters: impl Params,
) -> Result<EmittedPlan, DbError> {
    let operation = "explaining an emitted statement";
    let explained = format!("EXPLAIN QUERY PLAN {sql}");
    let mut statement = connection
        .prepare(&explained)
        .map_err(|error| error::sql(operation, error))?;
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
    })
}
