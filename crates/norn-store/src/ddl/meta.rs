//! The store's own pinned scalars, and what the `meta` table they sit in
//! holds besides.
//!
//! The table is a key/value one and its shape is `norn-db`'s, because it is
//! the one table whose shape may never change: the store schema fingerprint is
//! read out of it in order to decide whether the rest of the database is the
//! shape this build writes, so a reader of `meta` cannot rely on any other
//! table's columns being what it expects. Adding a pinned scalar is a new key
//! rather than a new column.
//!
//! `value` is declared `BLOB`, which in SQLite means the column has no
//! affinity and every value keeps the type it was written with: the
//! fingerprint reads back as text, a generation as an integer, and the pinned
//! vault schema as the bytes it was read from.
//!
//! # What lives here, and what lives under the seam
//!
//! **The mechanics keys are `norn-db`'s** — the pinned store schema version,
//! the DDL fingerprint, the schema digest taken at create, the write
//! generation and the store epoch. Every database derived over the substrate
//! records them, so one spelling of each belongs to the crate every such
//! database is opened through. This module names the keys that are the store's
//! own: `vault_schema_bytes`, `vault_schema_fingerprint` and
//! `vault_schema_generation` — the pinned vault-schema projection — and
//! `store_mode`, which is a fact about this crate's own two ways of opening
//! rather than about running a database.
//!
//! # The vault-schema projection is derived state
//!
//! The vault schema is a file, and **the file is its sole authority**. What
//! sits here is a projection of the bytes that were pinned at attach, kept so
//! that a derivation can record which schema it ran under and so that a
//! reader can tell that the schema moved without going back to the
//! filesystem. Nothing resolves a schema question by reading this: it answers
//! *which schema was this derived state derived under*, never *what does the
//! schema say*.

/// The pinned vault schema's bytes, exactly as they were read.
pub(crate) const VAULT_SCHEMA_BYTES: &str = "vault_schema_bytes";

/// The fingerprint of the pinned vault schema, which is the invalidation key
/// the schema-dependent tables carry.
pub(crate) const VAULT_SCHEMA_FINGERPRINT: &str = "vault_schema_fingerprint";

/// The write generation the vault schema was pinned at.
pub(crate) const VAULT_SCHEMA_GENERATION: &str = "vault_schema_generation";

/// Whether this store's file outlives the store — durable or throwaway.
///
/// The values and what a disagreement between the recorded one and the one an
/// open asks for means are [`crate::StoreMode`]'s and [`crate::Store`]'s: the
/// substrate opens a file and never decides whether that file survives the
/// handle.
pub(crate) const STORE_MODE: &str = "store_mode";
