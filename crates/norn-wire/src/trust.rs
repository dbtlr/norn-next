//! Where a vault entry stands between registration and a readable derived
//! state, and why that state is not readable when it is not.
//!
//! **Warming is polled.** Its counters are read as often as a caller wants to
//! read them, and nothing is pushed the other way: there is no subscription,
//! no event stream and no completion callback in this vocabulary. Progress is
//! a value a caller asks for.
//!
//! **There is deliberately no degraded-but-serving state.** An entry whose
//! derived state cannot be trusted is [`TrustState::Untrusted`] and refuses,
//! carrying its reason; it never answers from state it knows is stale, and it
//! never answers with a caveat attached.
//!
//! [`TrustState::Warming`], [`TrustState::Untrusted`] and
//! [`UntrustedReason::EnvironmentalRefusal`] are the variants whose payloads
//! grow — how far a heal has come, what made a state untrustworthy, what the
//! environment refused. Each is `#[non_exhaustive]` at the variant level and
//! built through its constructor, so a field arriving in one of them extends
//! the payload rather than breaking every caller that destructured it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A vault entry's trust state.
///
/// On the wire the state is an object tagged `state`: `{"state":"ready"}`,
/// `{"state":"warming","healed":12,"total_estimate":400}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TrustState {
    /// Registered, with no derived state to read from yet.
    Unattached,
    /// Attached and healing. The entry is not readable until the heal
    /// finishes.
    #[non_exhaustive]
    Warming {
        /// Documents the heal has finished with.
        healed: u64,
        /// Documents the heal expects to touch, and `null` when that estimate
        /// is not known yet. The count the heal finishes at may differ from
        /// it.
        total_estimate: Option<u64>,
    },
    /// Derived state is current, and reads answer from it.
    Ready,
    /// Derived state cannot be trusted. Requests against the entry refuse,
    /// carrying this reason.
    #[non_exhaustive]
    Untrusted {
        /// What made the state untrustworthy.
        reason: UntrustedReason,
    },
}

impl TrustState {
    /// An entry healing, `healed` documents in, against `total_estimate`
    /// documents expected — `None` while that estimate is not known yet.
    pub const fn warming(healed: u64, total_estimate: Option<u64>) -> Self {
        TrustState::Warming {
            healed,
            total_estimate,
        }
    }

    /// An entry whose derived state cannot be trusted, for `reason`.
    pub const fn untrusted(reason: UntrustedReason) -> Self {
        TrustState::Untrusted { reason }
    }
}

/// Why an entry's derived state cannot be trusted.
///
/// On the wire a reason is an object tagged `kind`:
/// `{"kind":"torn_increment"}`.
///
/// A refusal carries the same reason as its typed detail, so a caller reads
/// one vocabulary whether it polled the state or was refused by it.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum UntrustedReason {
    /// An increment stopped part-way, so derived state holds part of a change
    /// and no record of the rest.
    TornIncrement,
    /// The watcher overflowed or lost notifications, so what changed while it
    /// was not reporting is unknown.
    WatcherOverflow,
    /// The environment refused the work: the disk is full, or a vault path
    /// stopped being readable. The stored state is sound and the environment
    /// is not.
    #[non_exhaustive]
    EnvironmentalRefusal {},
}

impl UntrustedReason {
    /// The environment refused the work.
    pub const fn environmental_refusal() -> Self {
        UntrustedReason::EnvironmentalRefusal {}
    }
}
