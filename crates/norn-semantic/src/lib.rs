#![forbid(unsafe_code)]
//! The first lane-2 engine: a semantic sidecar converging on the lane-1
//! record.
//!
//! An **engine** is a domain that owns a **sidecar database** and derives
//! from committed lane-1 records, never from vault files ([ADR
//! 0021](../../../docs/decisions/0021-derived-indexes-split-into-two-lanes.md)).
//! This crate is that shape, proven end to end with the deterministic stub
//! embedder: it consumes the store's change feed through consumer-owned
//! cursors, embeds changed bodies through [`norn_embed`], retracts deaths,
//! and answers vector-nearest over what it holds.
//!
//! # The sidecar is a second database, not a second schema
//!
//! One sidecar per engine per vault, opened through [`norn_db`] as its second
//! client ([ADR 0022](../../../docs/decisions/0022-one-crate-knows-sql.md)):
//! its own DDL fingerprint, its own epoch, its own damage and rebuild
//! domain. Rows are keyed by `(path, model id, model version)` and carry the
//! `body_hash` they were computed over — content-addressed, so the cheap
//! state's rebuild never forces the expensive state's recompute, and never
//! keyed by a main-database rowid, so the two databases share no lifetime.
//!
//! # Eventual consistency is the stated contract
//!
//! The feed is a query over current rows and tombstones, ordered by the
//! store's write generation. A cursor is valid within one store epoch; an
//! epoch that moved means the store was rebuilt, and the engine reconciles
//! and rescans — recomputing only what actually changed. A missed wake costs
//! latency, never correctness: every drain converges on the same settled
//! state, and the convergence bar holds that state to exact equality with a
//! from-zero recompute over current lane-1 rows.
//!
//! # The inference firewall
//!
//! What this crate derives answers queries and nothing else. No edge from
//! here reaches findings, plans, or repair — the crates that decide those
//! cannot name this one — and the engine's one store surface is
//! [`norn_store::FeedRead`], a handle with no write verb, so inferred state
//! cannot enter the tables the correctness path trusts. Inferred state can
//! therefore be wrong, stale, or model-versioned without any of that
//! becoming a correctness input.
//!
//! # Who calls this
//!
//! The host's composition: `norn-host` opens one engine per enabled vault at
//! config delivery — the enable act — relays a drain after each leg that
//! commits lane-1 work, and answers vector-nearest through its semantic
//! capability. [`Settings`] is the engine owning what its
//! `[engine.semantic]` section means; the host hands the table over and
//! defines nothing past the handoff. What does not exist yet is the layer
//! that composes that capability into a running product — the serving
//! surface, whose verbs the verb charter owns — so the call graph above this
//! crate today is the host's own suite. An unreached path here routes to
//! that layer, not to removal.
//!
//! # Where to start
//!
//! - [`Engine`] — open, drain, nearest; the whole surface.
//! - [`DrainReport`] — what one drain did, and [`DrainReport::is_settled`].
//! - [`SidecarOutcome`] — how an open ended up with its database.

mod ddl;
mod engine;
mod error;
mod settings;
mod sidecar;

pub use engine::{DrainReport, Engine, Neighbor, VectorRow};
pub use error::EngineError;
pub use settings::{SectionError, Settings};
pub use sidecar::SidecarOutcome;
