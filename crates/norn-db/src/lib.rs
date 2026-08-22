#![forbid(unsafe_code)]
//! The mechanics of running a SQLite database, and nothing about what one
//! holds.
//!
//! **No other crate opens a SQLite connection.** This is the substrate seam:
//! connection ownership and the pragmas a schema is designed to be read under,
//! the pinned-scalar `meta` pattern, the DDL fingerprint and the schema digest
//! that answer whether a database is still the shape a build wrote, the store
//! epoch a database carries from creation to discard, the immediate
//! transaction every write takes, damage typing at the driver seam, the
//! `EXPLAIN` plan handout, and the database file's own lifecycle.
//!
//! # A client owns the meaning; this crate owns the machinery
//!
//! A statement list arrives from the crate that owns what the statements mean,
//! and comes back out as a fingerprint. Nothing here reads a document, a vault,
//! a wire type or a lane, and no verdict about a client's schema is taken here:
//! this crate opens, mints, digests, hands back and removes, and whether a
//! disagreement is a rebuild is read one layer up.
//!
//! # Where to start
//!
//! - [`connect`] — the one place a connection is opened, and [`Database`], the
//!   handle that binds an open connection to its file and its epoch.
//! - [`meta`] — the pinned scalars an open reads before it trusts anything
//!   else, the mechanics keys among them, and the read and write of one.
//! - [`digest`] and [`schema_digest`] — which statement list produced this
//!   database, and whether it still holds what that list created.
//! - [`Database::immediate_transaction`] — the transaction discipline every
//!   write goes through, and [`Database::deferred_transaction`], the read
//!   snapshot a multi-statement read answers from.
//! - [`sql`] and [`is_damaged`] — the one judgment made about a driver error:
//!   damaged state, which authorizes a rebuild, or a broken environment, which
//!   does not.
//! - [`emitted_plan`] — the plan SQLite reported for a statement a client
//!   emitted, as plain data a bar can be asserted against.
//! - [`prepare_parent`], [`remove_database`] — the parts of a database file's
//!   lifecycle the driver does not cover.
//!
//! # The driver is re-exported rather than declared twice
//!
//! A client composes SQL, so it names driver types. It reaches them through
//! [`rusqlite`] here rather than declaring the dependency for itself, which is
//! what makes "one crate knows SQL" a fact about the manifests and not only
//! about the code.

pub use rusqlite;

mod database;
mod error;
#[cfg(feature = "induced-failure")]
pub mod faults;
pub mod meta;
mod plan;
mod schema;

pub use database::{Attempt, Database, connect, mint_an_epoch, prepare_parent, remove_database};
pub use error::{DbError, damage_or_fail, is_damaged, sql, sql_at_statement};
pub use plan::{EmittedPlan, PlanStep, emitted_plan};
pub use schema::{digest, schema_digest};
