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
//! and the [`UntrustedReason`] it carries — the one shape a refusal takes:
//! [`ErrorEnvelope`], with its [`ReasonCode`] and [`ErrorDetail`]; and what a
//! finding is filed under, [`FindingKind`]. Maintainer contention carries the
//! diagnostic [`MaintainerIdentity`] reported by the lock without changing an
//! entry's trust state.
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
//!   and variant names on the wire — except the code registries, whose members
//!   are renamed to the `namespace/what-happened` grammar below. One read path
//!   is written by hand —
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
//! string, as [`ReasonCode`], [`FindingKind`], [`WatcherLossCause`] and
//! [`WarmingPhase`] are; an enum whose variants may carry a payload is a
//! tagged object, as [`TrustState`], [`UntrustedReason`] and [`ErrorDetail`]
//! are. The flat string keeps a code matchable as a value; the tagged object
//! keeps a payload's arrival from changing what the value is. Two of those
//! four flat strings are code registries; [`WatcherLossCause`] and
//! [`WarmingPhase`] are not codes but nested bare-string values under `cause`
//! and `phase`, carrying no namespace.
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
//! [`UntrustedReason::environmental_refusal`], [`ErrorDetail::duplicate_root`],
//! [`ErrorDetail::entry_untrusted`], [`ErrorDetail::maintainer_contended`],
//! [`ErrorDetail::unknown_vault`], [`MaintainerIdentity::named`] and
//! [`MaintainerIdentity::unknown`].
//!
//! **What `#[non_exhaustive]` protects is Rust destructuring, not a writer's
//! bytes.** A field added to a payload is a field the read path requires, so
//! JSON an older writer produced — `{"kind":"environmental_refusal"}` once
//! `detail` exists — fails the read as a missing field. Growth is a promise to
//! Rust callers and to readers of the payload a current writer emits;
//! compatibility with bytes an older writer produced is not promised before
//! 1.0.
//!
//! **A struct tolerates a field it does not know; an enum refuses a variant it
//! does not know.** A reader drops an unknown field, so a writer that gained
//! one is still read by a reader that has not. A reader handed a tag or a code
//! string it does not know fails the read instead: there is no
//! `#[serde(other)]` catch-all anywhere in the vocabulary, because a variant
//! nobody can interpret is a refusal to parse rather than a value to pass on
//! degraded.
//!
//! # The code grammar, and what is not a code
//!
//! **A code is a flat `namespace/what-happened` string**, lowercase kebab-case
//! on both sides of one slash. Codes are what a client enumerates, switches on
//! and filters by, and they live in exactly two closed registries:
//! [`ReasonCode`] for what the host refused (`host/…`), and [`FindingKind`]
//! for what a finding is filed under (`document/…`). A namespace names who the
//! fact is about, never which crate produced it, and a code is *defined*
//! nowhere but here: a layer below stores the code it was handed rather than
//! defining one, and a surface that needs a code it cannot find adds it to a
//! registry rather than spelling a string of its own.
//!
//! **A nested typed reason is structure inside a code, not a code.** The
//! reason a `host/entry-untrusted` detail carries is [`UntrustedReason`], an
//! object whose `kind` tag is a `snake_case` string under the detail's own
//! `reason` key; the [`WatcherLossCause`] inside it is a `snake_case` string
//! that is the value of `cause` rather than a tag. Both are read after the
//! code has been matched, so they carry no namespace, never appear in a code
//! list, and are not values a client dispatches on before it knows the code.
//! Structure grows there without the code list growing. [`WarmingPhase`] sits
//! the same way inside [`TrustState`]: a `snake_case` value under `phase`,
//! read after the `state` tag has been matched.
//!
//! **A note a layer raises about its own reading is not a code.** The text
//! layer files a parse diagnostic under a kebab-case identifier of its own:
//! it names what a reader worked around inside one document, it carries no
//! namespace, and it does not cross this seam. What crosses is what a consumer
//! derives from such notes — a count on a document's row, or a finding — and a
//! finding is filed under [`FindingKind`] like every other.
//!
//! **A `detail` string is prose and never a match target.** Where a payload
//! carries one — an environmental refusal, a lost watcher — it exists for a
//! person reading a message or a log. Its wording is not contracted, so a
//! client that branches on its text is branching on something free to change;
//! the code and the typed fields beside it are what carry the decision. A
//! detail is diagnostic text for an operator and may name machine-local paths
//! — a store file, a lock file, a vault root — because naming what the machine
//! refused is the point of it. It carries nothing beyond that account of the
//! failure, and it is never content a client parses.
//!
//! # Minting a reason code
//!
//! Each [`ReasonCode`] pairs with exactly one [`ErrorDetail`] variant: the
//! detail's wire tag *is* the code string, and [`ErrorDetail::code`] hands back
//! the code the detail belongs to.

mod error;
mod finding;
mod trust;

pub use error::{ErrorDetail, ErrorEnvelope, MaintainerIdentity, ReasonCode};
pub use finding::{FindingKind, UnknownFindingKind};
pub use trust::{TrustState, UntrustedReason, WarmingPhase, WatcherLossCause};
