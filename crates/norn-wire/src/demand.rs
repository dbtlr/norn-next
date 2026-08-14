//! What a demand asks for, apart from the vault it names.
//!
//! A demand carries one parameter beyond its name: how the derived state it
//! asks for is held. That is a request parameter and nothing else — it is
//! read before an entry is touched and it decides nothing about what an entry
//! already holds — so it is a closed vocabulary whose members carry no data,
//! which makes it a flat string on the wire.
//!
//! The host refuses a mode it holds no lifecycle for, and that refusal is
//! coded like every other: [`ReasonCode::HostUnsupportedAttachMode`] with the
//! refused mode as its typed detail, so a client branches on the mode rather
//! than on prose.
//!
//! [`ReasonCode::HostUnsupportedAttachMode`]: crate::ReasonCode::HostUnsupportedAttachMode

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How the derived state a demand asks for is held.
///
/// On the wire a mode is the flat string itself: `"durable"`, `"throwaway"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AttachMode {
    /// A durable database, watcher coverage, and trust that warms and stays
    /// warm: what every registered vault is served under.
    Durable,
    /// Disposable derivation over a throwaway store, discarded with the work
    /// that asked for it. A demand naming this mode is refused, coded
    /// `host/unsupported-attach-mode` and carrying the mode back.
    Throwaway,
}
