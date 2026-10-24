//! Private lab transaction grouping. Units retain their bounded write bodies.
//! No trace, authorizer, WAL hook, pager setting, or checkpoint is installed here.
use std::ops::Deref;

use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::{Database, DbError, sql};

/// A commit is charged to the phase of its last completed unit. A group may
/// cross phase boundaries; these are real commits, not per-phase estimates.
#[derive(Clone, Copy, Debug, Default)]
pub enum Phase {
    #[default]
    Documents,
    HealthReset,
    HealthGeneration,
    Drain,
}

/// Scalar-only diagnostics, separate from ordinary derivation counters.
#[derive(Clone, Debug, Default)]
pub struct Receipt {
    pub group: usize,
    pub commit_attempts: u64,
    pub commits: u64,
    pub rollback_attempts: u64,
    pub rollbacks: u64,
    pub automatic_rollbacks: u64,
    pub document_changesets: u64,
    pub health_resets: u64,
    pub health_generation_groups: u64,
    pub drain_changesets: u64,
    pub commits_documents: u64,
    pub commits_health_reset: u64,
    pub commits_health_generation: u64,
    pub commits_drain: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Control {
    pub(crate) used: bool,
    pub(crate) active: bool,
    pub(crate) failed: bool,
    recording: bool,
    pending: usize,
    last_phase: Phase,
    receipt: Receipt,
    cleanup_error: Option<DbError>,
}

pub(crate) fn refusal(message: &str) -> DbError {
    DbError::Sql {
        operation: "controlling lab construction",
        condition: None,
        message: message.into(),
    }
}

use crate::require_transaction;

impl Control {
    fn committed(&mut self, phase: Phase) {
        self.receipt.commits += 1;
        match phase {
            Phase::Documents => self.receipt.commits_documents += 1,
            Phase::HealthReset => self.receipt.commits_health_reset += 1,
            Phase::HealthGeneration => self.receipt.commits_health_generation += 1,
            Phase::Drain => self.receipt.commits_drain += 1,
        }
        self.pending = 0;
    }

    fn abandon(&mut self, connection: &Connection) -> Result<(), DbError> {
        self.failed = true;
        self.active = false;
        if !connection.is_autocommit() {
            self.receipt.rollback_attempts += 1;
            if let Err(error) = connection.execute_batch("ROLLBACK") {
                let error = sql("abandoning lab construction", error);
                self.cleanup_error = Some(error.clone());
                return Err(error);
            }
            self.receipt.rollbacks += 1;
        } else if self.pending != 0 {
            self.receipt.automatic_rollbacks += 1;
        }
        self.pending = 0;
        if let Some(error) = &self.cleanup_error {
            return Err(error.clone());
        }
        Ok(())
    }
}

impl Database {
    /// Start once on this handle. The domain must first commit its nonadoption marker.
    /// 1 is the ordinary-transaction control, 16 is bounded, 0 is diagnostic unbounded.
    pub fn begin_construction(&mut self, group: usize) -> Result<(), DbError> {
        if !matches!(group, 0 | 1 | 16) {
            return Err(refusal(
                "construction group must be 1, 16, or unbounded (0)",
            ));
        }
        if self.construction.used || !self.connection.is_autocommit() {
            return Err(refusal(
                "construction requires a fresh first attempt in autocommit",
            ));
        }
        self.construction.used = true;
        self.construction.active = true;
        self.construction.recording = true;
        self.construction.receipt.group = group;
        // Open lazily at the first unit, never a speculative empty transaction.
        Ok(())
    }

    /// A normal Rust Transaction owns ordinary units (including control group 1).
    /// Grouped units borrow the connection; their Drop explicitly abandons the
    /// entire group on any error or unwind, preserving rollback-on-Drop.
    pub fn construction_unit(
        &mut self,
        phase: Phase,
        operation: &'static str,
    ) -> Result<Unit<'_>, DbError> {
        let control = &mut self.construction;
        if control.failed {
            return Err(refusal("construction was abandoned"));
        }
        let phase = if control.used && !control.active {
            Phase::Drain
        } else {
            phase
        };
        let grouped = control.active && control.receipt.group != 1;
        let transaction = if grouped {
            if control.pending == 0 {
                if !self.connection.is_autocommit() {
                    control.abandon(&self.connection)?;
                    return Err(refusal("unexpected transaction at a new group"));
                }
                if let Err(error) = self.connection.execute_batch("BEGIN IMMEDIATE") {
                    control.abandon(&self.connection)?;
                    return Err(sql(operation, error));
                }
            } else if let Err(error) = require_transaction(&self.connection) {
                control.abandon(&self.connection)?;
                return Err(error);
            }
            Body::Grouped(&self.connection)
        } else {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| {
                    if control.recording {
                        control.failed = true;
                        control.active = false;
                    }
                    sql(operation, error)
                })?;
            Body::Ordinary(Some(transaction))
        };
        Ok(Unit {
            transaction,
            control,
            phase,
            finished: false,
        })
    }

    /// End before watcher reconciliation. A partial group commits here, charged
    /// to its last unit's phase. No new transaction is opened afterward.
    pub fn finish_construction(&mut self) -> Result<(), DbError> {
        if !self.construction.active || self.construction.failed {
            return Err(refusal("no successful construction is active"));
        }
        if self.construction.pending > 0 {
            if let Err(error) = require_transaction(&self.connection) {
                self.construction.abandon(&self.connection)?;
                return Err(error);
            }
            self.construction.receipt.commit_attempts += 1;
            if let Err(error) = self.connection.execute_batch("COMMIT") {
                self.construction.abandon(&self.connection)?;
                return Err(sql("committing final construction group", error));
            }
            self.construction.committed(self.construction.last_phase);
        }
        self.construction.active = false;
        self.ensure_construction_complete()
    }

    /// Explicit error cleanup also surfaces rollback failures retained by Drop.
    pub fn abandon_construction(&mut self) -> Result<(), DbError> {
        self.construction.abandon(&self.connection)
    }

    /// Publication/marker removal cannot run inside any transaction or after failure.
    pub fn ensure_construction_complete(&self) -> Result<(), DbError> {
        if self.construction.active || self.construction.failed || !self.connection.is_autocommit()
        {
            return Err(refusal("construction is active or abandoned"));
        }
        Ok(())
    }

    /// Stop the private drain account at the marker's existing completion
    /// boundary. Later warm units retain ordinary error and retry semantics.
    pub fn seal_construction_receipt(&mut self) -> Result<(), DbError> {
        self.ensure_construction_complete()?;
        self.construction.recording = false;
        Ok(())
    }

    pub fn construction_receipt(&self) -> Option<&Receipt> {
        self.construction.used.then_some(&self.construction.receipt)
    }

    pub fn refuse_unconverted_construction_writer(&mut self) -> Result<(), DbError> {
        if self.construction.active || self.construction.failed {
            self.construction.abandon(&self.connection)?;
            return Err(refusal("unconverted writer during lab construction"));
        }
        Ok(())
    }
}

enum Body<'a> {
    Ordinary(Option<Transaction<'a>>),
    Grouped(&'a Connection),
}

/// Transaction authority for exactly one existing bounded write unit.
pub struct Unit<'a> {
    transaction: Body<'a>,
    control: &'a mut Control,
    phase: Phase,
    finished: bool,
}

impl Deref for Unit<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        match &self.transaction {
            Body::Ordinary(Some(transaction)) => transaction,
            Body::Grouped(connection) => connection,
            Body::Ordinary(None) => unreachable!("consumed transaction"),
        }
    }
}

impl Unit<'_> {
    pub fn commit(mut self) -> Result<(), DbError> {
        require_transaction(&self)?;
        let tracking = self.control.recording;
        if tracking {
            match self.phase {
                Phase::Documents => self.control.receipt.document_changesets += 1,
                Phase::HealthReset => self.control.receipt.health_resets += 1,
                Phase::HealthGeneration => self.control.receipt.health_generation_groups += 1,
                Phase::Drain => self.control.receipt.drain_changesets += 1,
            }
        }
        match &mut self.transaction {
            Body::Ordinary(transaction) => {
                if tracking {
                    self.control.receipt.commit_attempts += 1;
                }
                transaction
                    .as_ref()
                    .expect("live transaction")
                    .execute_batch("COMMIT")
                    .map_err(|error| sql("committing a write unit", error))?;
                if tracking {
                    self.control.committed(self.phase);
                }
            }
            Body::Grouped(connection) => {
                self.control.pending += 1;
                self.control.last_phase = self.phase;
                if self.control.receipt.group != 0
                    && self.control.pending == self.control.receipt.group
                {
                    self.control.receipt.commit_attempts += 1;
                    connection
                        .execute_batch("COMMIT")
                        .map_err(|error| sql("committing a construction group", error))?;
                    self.control.committed(self.phase);
                }
            }
        }
        self.finished = true;
        Ok(())
    }
}

impl Drop for Unit<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        match &mut self.transaction {
            Body::Ordinary(transaction) => {
                // Keep rusqlite's rollback-on-Drop even on commit failure.
                if let Some(transaction) = transaction.take() {
                    if transaction.is_autocommit() {
                        if self.control.recording {
                            self.control.receipt.automatic_rollbacks += 1;
                        }
                        // SQLite already abandoned the unit. Transaction's own
                        // Drop sees autocommit and has no rollback left to run.
                        drop(transaction);
                        if self.control.recording {
                            self.control.failed = true;
                            self.control.active = false;
                        }
                        return;
                    }
                    if self.control.recording {
                        self.control.receipt.rollback_attempts += 1;
                    }
                    let result = transaction.rollback();
                    if self.control.recording {
                        match result {
                            Ok(()) => self.control.receipt.rollbacks += 1,
                            Err(error) => {
                                self.control.cleanup_error =
                                    Some(sql("rolling back a write unit", error))
                            }
                        }
                    }
                }
                if self.control.recording {
                    self.control.failed = true;
                    self.control.active = false;
                }
            }
            Body::Grouped(connection) => {
                // Drop cannot return errors; explicit host cleanup reports any
                // rollback failure retained by abandon.
                if connection.is_autocommit() {
                    self.control.receipt.automatic_rollbacks += 1;
                    self.control.pending = 0;
                }
                let _ = self.control.abandon(connection);
            }
        }
    }
}
