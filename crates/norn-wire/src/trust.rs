//! Where a vault entry stands between registration and a readable derived
//! state, and why that state is not readable when it is not.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A vault entry's trust state.
///
/// On the wire the state is an object tagged `state`: `{"state":"ready"}`,
/// `{"state":"warming","healed":12,"total_estimate":400}`.
///
/// **Warming is polled.** The two counters are read as often as a caller
/// wants to read them, and nothing is pushed the other way: there is no
/// subscription, no event stream and no completion callback in this
/// vocabulary. Progress is a value a caller asks for.
///
/// **There is deliberately no degraded-but-serving state.** An entry whose
/// derived state cannot be trusted is [`TrustState::Untrusted`] and refuses
/// carrying its reason; it never answers from state it knows is stale, and it
/// never answers with a caveat attached. The enum is `#[non_exhaustive]`, and
/// that is the door a further state comes through.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TrustState {
    /// Registered and not attached. Attach is lazy: the first request that
    /// touches the entry triggers it.
    Unattached,
    /// Attached and healing. The entry is not readable until the heal
    /// finishes.
    Warming {
        /// Documents the heal has finished with.
        healed: u64,
        /// Documents the heal expects to touch. An estimate, and the count it
        /// finishes at may differ.
        total_estimate: u64,
    },
    /// Derived state is current, and reads answer from it.
    Ready,
    /// Derived state cannot be trusted. Requests against the entry refuse,
    /// carrying this reason.
    Untrusted {
        /// What made the state untrustworthy.
        reason: UntrustedReason,
    },
}

/// Why an entry's derived state cannot be trusted.
///
/// On the wire a reason is an object tagged `kind`:
/// `{"kind":"torn_increment"}`.
///
/// The same value is what a refusal carries as its typed detail — see
/// [`ErrorDetail::EntryUntrusted`](crate::ErrorDetail::EntryUntrusted) — so a
/// caller reads one vocabulary whether it polled the state or was refused by
/// it.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum UntrustedReason {
    /// An increment stopped part-way, so derived state holds part of a change
    /// and no record of the rest. A partial increment is never treated as
    /// complete: the entry re-heals.
    TornIncrement,
    /// The watcher overflowed or lost notifications, so what changed while it
    /// was not reporting is unknown. The gap is never absorbed — the entry
    /// re-heals against the files.
    WatcherOverflow,
    /// The environment refused the work: the disk is full, or a vault path
    /// stopped being readable. The stored state is sound and the environment
    /// is not, so the entry stays untrusted and nothing is discarded.
    EnvironmentalRefusal,
}
