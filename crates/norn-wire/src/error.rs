//! The one structured error envelope: a reason code, a message, and the typed
//! detail the code carries.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::trust::UntrustedReason;

/// The code a refusal is filed under.
///
/// A code is a flat namespaced string on the wire — `namespace/what-happened`
/// — and a Rust variant in here. The enum, rather than a set of string
/// constants, is what makes a code a value the compiler checks: a producer
/// cannot emit a code that does not exist, and a consumer switching on codes
/// is told when the list grows.
///
/// The list holds the codes the system can emit today, and it grows one code
/// at a time with the mechanism that emits it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub enum ReasonCode {
    /// `host/entry-untrusted` — the vault entry's derived state cannot be
    /// trusted, so the request is refused rather than answered. The detail is
    /// the [`UntrustedReason`].
    #[serde(rename = "host/entry-untrusted")]
    HostEntryUntrusted,
}

impl ReasonCode {
    /// The exact string this code is on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            ReasonCode::HostEntryUntrusted => "host/entry-untrusted",
        }
    }
}

/// The typed payload one reason code carries.
///
/// One variant per code, and the variant's wire tag *is* the code:
/// `{"code":"host/entry-untrusted","reason":{"kind":"torn_increment"}}`. That
/// is what makes a detail readable on its own and what makes
/// [`ErrorDetail::code`] a lookup rather than a table somebody maintains.
///
/// A detail composes the types the rest of the vocabulary already uses — an
/// untrusted entry's detail is the same [`UntrustedReason`] its
/// [`TrustState`](crate::TrustState) carries — rather than flattening those
/// distinctions into more codes.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "code")]
#[non_exhaustive]
pub enum ErrorDetail {
    /// The detail of [`ReasonCode::HostEntryUntrusted`].
    #[serde(rename = "host/entry-untrusted")]
    EntryUntrusted {
        /// Why the entry's derived state cannot be trusted.
        reason: UntrustedReason,
    },
}

impl ErrorDetail {
    /// The code this detail is the payload of.
    pub const fn code(&self) -> ReasonCode {
        match self {
            ErrorDetail::EntryUntrusted { .. } => ReasonCode::HostEntryUntrusted,
        }
    }
}

/// A refusal, in the one shape every refusal takes.
///
/// Three fields and no others: the [`code`](ErrorEnvelope::code) a program
/// switches on, the [`message`](ErrorEnvelope::message) a person reads, and
/// the [`detail`](ErrorEnvelope::detail) the code pairs with.
///
/// **There is deliberately no `retryable` flag.** Whether an operation is
/// worth trying again follows from the code and the detail, and a boolean
/// beside them is a second answer to that question that can disagree with the
/// first.
///
/// **There is deliberately no forecast field.** A forecast is a report about
/// what a mutation would do; it is its own wire type, requested and returned
/// in its own right, and riding it on a refusal would make every refusal carry
/// a shape only some refusals have.
///
/// The struct is `#[non_exhaustive]`, so it extends by gaining a field and
/// [`ErrorEnvelope::new`] is how it is built.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ErrorEnvelope {
    /// What was refused.
    pub code: ReasonCode,
    /// The refusal in words, for a person.
    pub message: String,
    /// The code's typed payload.
    pub detail: ErrorDetail,
}

impl ErrorEnvelope {
    /// An envelope carrying `detail`, under the code that detail belongs to.
    ///
    /// The code is taken from the detail rather than passed in, so the two
    /// name the same refusal.
    pub fn new(message: impl Into<String>, detail: ErrorDetail) -> Self {
        ErrorEnvelope {
            code: detail.code(),
            message: message.into(),
            detail,
        }
    }
}
