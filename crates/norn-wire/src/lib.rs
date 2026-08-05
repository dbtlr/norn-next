#![forbid(unsafe_code)]
//! The vocabulary. Pure types: no I/O, no logic.
//!
//! What crosses the client/host seam is defined here exactly once, and every
//! surface is a derived rendering of it: CLI flags, MCP tool schemas and HTTP
//! payloads render these types and never define vocabulary of their own. The
//! crate links nothing else in the workspace and reaches no filesystem, no
//! database and no socket — a type in here can be constructed, serialized and
//! compared, and that is the whole of what it does.
//!
//! What is defined here today is where a vault entry stands — [`TrustState`]
//! and the [`UntrustedReason`] it carries — and the one shape a refusal takes:
//! [`ErrorEnvelope`], with its [`ReasonCode`] and [`ErrorDetail`]. Maintainer
//! contention carries the diagnostic [`MaintainerIdentity`] reported by the
//! lock without changing an entry's trust state.
//!
//! Nothing crosses the seam that is not a type from here. There is no untyped
//! JSON value in any signature and no JSON-in-a-string; a payload that cannot
//! be spelled as a type here does not cross.
//!
//! # Derive discipline
//!
//! Every public type carries the same derives, and the reason is that a wire
//! type is read by serde and described by schemars at once — a type that
//! serializes but has no schema is a payload no surface can advertise:
//!
//! - [`serde::Serialize`] and [`serde::Deserialize`], with `snake_case` field
//!   and variant names on the wire. One read path is written by hand —
//!   [`ErrorEnvelope`] refuses a parse whose code is not its detail's — with
//!   the same wire shape a derive would read.
//! - [`schemars::JsonSchema`], which reads the same serde attributes, so the
//!   advertised schema and the emitted bytes are one description.
//! - `Debug`, `Clone` and `PartialEq`, plus `Eq` wherever every field holds it.
//!
//! **Enums are internally tagged with an explicit tag name, never externally
//! tagged.** An externally tagged enum makes the variant name a JSON key, so a
//! reader has to enumerate keys to learn what it is holding and a new variant
//! changes the object's shape rather than one field's value. The tag names are
//! part of the wire: [`TrustState`] is tagged `state`, [`UntrustedReason`] is
//! tagged `kind`, and [`ErrorDetail`] is tagged `code`.
//!
//! **A tagged enum's variants are struct-shaped.** Internal tagging merges the
//! tag into the variant's own map, so a newtype variant fails at serialize
//! time while schemars advertises a schema saying it works: the break arrives
//! at runtime, against a shape a consumer was told to expect. A variant that
//! carries data names its fields.
//!
//! **A tagged object or a flat string** is decided by whether the variants
//! carry data. A closed vocabulary whose members carry nothing is a flat
//! string, as [`ReasonCode`] and [`WatcherLossCause`] are; an enum whose
//! variants may carry a payload is
//! a tagged object, as [`TrustState`], [`UntrustedReason`] and [`ErrorDetail`]
//! are. The flat string keeps a code matchable as a value; the tagged object
//! keeps a payload's arrival from changing what the value is.
//!
//! **Doc comments on a type, a variant or a field are published.** schemars
//! lifts them verbatim into the schema `description`s an MCP consumer reads,
//! so they carry wire documentation and nothing else: no Rust intralinks, no
//! maintainer rationale, no narration of shapes the type does not have.
//! Rationale belongs in module documentation such as this, which schemars does
//! not lift.
//!
//! # Extension, and what a version skew does
//!
//! Public enums are `#[non_exhaustive]`, and so is [`ErrorEnvelope`], which
//! extends by gaining a field. A variant that carries a payload is
//! `#[non_exhaustive]` in its own right, so the payload extends by gaining a
//! field too rather than by breaking every caller that destructured it.
//! `#[non_exhaustive]` binds across crates, so a shape consumers cannot write
//! as a literal carries a constructor: [`ErrorEnvelope::new`],
//! [`TrustState::warming`], [`TrustState::untrusted`],
//! [`UntrustedReason::watcher_lost`],
//! [`UntrustedReason::environmental_refusal`] and
//! [`ErrorDetail::entry_untrusted`].
//!
//! **A struct tolerates a field it does not know; an enum refuses a variant it
//! does not know.** A reader drops an unknown field, so a writer that gained
//! one is still read by a reader that has not. A reader handed a tag or a code
//! string it does not know fails the read instead: there is no
//! `#[serde(other)]` catch-all anywhere in the vocabulary, because a variant
//! nobody can interpret is a refusal to parse rather than a value to pass on
//! degraded.
//!
//! # Minting a reason code
//!
//! Each [`ReasonCode`] pairs with exactly one [`ErrorDetail`] variant: the
//! detail's wire tag *is* the code string, and [`ErrorDetail::code`] hands back
//! the code the detail belongs to.

mod error;
mod trust;

pub use error::{ErrorDetail, ErrorEnvelope, MaintainerIdentity, ReasonCode};
pub use trust::{TrustState, UntrustedReason, WatcherLossCause};
