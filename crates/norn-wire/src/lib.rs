#![forbid(unsafe_code)]
//! The vocabulary. Pure types: no I/O, no logic.
//!
//! Request params, reports, typed plans, findings and trust states are defined
//! here exactly once, and every surface is a derived rendering of them: CLI
//! flags, MCP tool schemas and HTTP payloads render these types and never
//! define vocabulary of their own. The crate links nothing else in the
//! workspace and reaches no filesystem, no database and no socket — a type in
//! here can be constructed, serialized and compared, and that is the whole of
//! what it does.
//!
//! Nothing crosses the client/host seam that is not a type from here. There is
//! no untyped JSON value in any signature and no JSON-in-a-string; a payload
//! that cannot be spelled as a type here does not cross.
//!
//! # Derive discipline
//!
//! Every public type carries the same derives, and the reason is that a wire
//! type is read by serde and described by schemars at once — a type that
//! serializes but has no schema is a payload no surface can advertise:
//!
//! - [`serde::Serialize`] and [`serde::Deserialize`], with `snake_case` field
//!   and variant names on the wire.
//! - [`schemars::JsonSchema`], which reads the same serde attributes, so the
//!   advertised schema and the emitted bytes are one description.
//! - `Debug`, `Clone` and `PartialEq`, plus `Eq` wherever every field holds it.
//!
//! **Enums are internally tagged with an explicit tag name, never externally
//! tagged.** An externally tagged enum makes the variant name a JSON key, so a
//! reader has to enumerate keys to learn what it is holding and a new variant
//! changes the object's shape rather than one field's value. The tag names are
//! part of the wire: [`TrustState`] is tagged `state`, [`UntrustedReason`] is
//! tagged `kind`, and [`ErrorDetail`] is tagged `code`. A code list such as
//! [`ReasonCode`] carries no data and is a flat string.
//!
//! Public enums are `#[non_exhaustive]`, and so is [`ErrorEnvelope`], which
//! extends by gaining fields. `#[non_exhaustive]` binds across crates, so a
//! type consumers cannot write as a literal carries a constructor —
//! [`ErrorEnvelope::new`] is the one such type today.
//!
//! # Minting a reason code
//!
//! [`ReasonCode`] is a Rust enum rather than a set of string constants, and it
//! holds exactly one code: `host/entry-untrusted`. **A code is minted by the
//! task that brings the mechanism producing it** — the content-addressed
//! store, the filesystem seam, the derived store and the embedding runtime
//! each mint theirs when they land, together with the typed detail that code
//! carries. A code minted ahead of its producer describes a refusal nothing
//! can emit, and a code list nobody can exercise drifts from what the system
//! actually refuses with.
//!
//! Each code pairs with one [`ErrorDetail`] variant, and the pairing is the
//! shape rather than a convention: the detail's wire tag *is* the code string,
//! and [`ErrorDetail::code`] hands back the code the detail belongs to.

mod error;
mod trust;

pub use error::{ErrorDetail, ErrorEnvelope, ReasonCode};
pub use trust::{TrustState, UntrustedReason};
