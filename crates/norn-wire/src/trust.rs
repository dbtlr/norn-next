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
//! [`TrustState::Warming`], [`TrustState::Untrusted`],
//! [`UntrustedReason::WatcherLost`] and [`UntrustedReason::EnvironmentalRefusal`]
//! are the variants whose payloads grow — how far a heal has come, what made a
//! state untrustworthy, what ended watcher coverage, what the environment
//! refused. Each is `#[non_exhaustive]` at the variant level, so a field
//! arriving in one of them is not a compile error for a caller that
//! destructured it: a pattern over such a variant already has to accept the
//! fields it does not name. Construction is what the arriving field does
//! break, and deliberately — the constructors take their fields positionally,
//! so a payload that grows one is a signature change every producer is made to
//! answer for rather than a default nobody chose.
//!
//! **Warming names what it is doing, not only how far it has come.** An entry
//! reaches readable state through work of two kinds — installing change
//! detection over the vault, then deriving documents — and only the second of
//! them has anything to count. Counters alone therefore cannot tell a heal
//! that has not started from one that is not progressing: both read as zero
//! healed against an unknown total. [`WarmingPhase`] is what separates them,
//! so a caller waiting on an entry, and an operator reading why a wait
//! expired, learn which of the two the entry is in.
//!
//! **Two watcher reasons, split by who resumes.** [`UntrustedReason::WatcherOverflow`]
//! is a running watcher that reported less than it saw, and the entry re-heals
//! itself; [`UntrustedReason::WatcherLost`] is coverage that ended, and the
//! entry re-heals when a client demands it. One word for both would make a
//! state that recovers on its own indistinguishable from one that waits.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A vault entry's trust state.
///
/// On the wire the state is an object tagged `state`: `{"state":"ready"}`,
/// `{"state":"warming","phase":"healing","healed":12,"total_estimate":400}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TrustState {
    /// Registered, with no derived state to read from yet.
    Unattached,
    /// Attached and working toward readable derived state. The entry is not
    /// readable until that work finishes.
    #[non_exhaustive]
    Warming {
        /// The kind of work the entry is doing.
        phase: WarmingPhase,
        /// Documents the heal has finished with. It stands at zero until the
        /// heal begins.
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
    /// An entry warming in `phase`, `healed` documents in, against
    /// `total_estimate` documents expected — `None` while that estimate is not
    /// known yet.
    pub const fn warming(phase: WarmingPhase, healed: u64, total_estimate: Option<u64>) -> Self {
        TrustState::Warming {
            phase,
            healed,
            total_estimate,
        }
    }

    /// An entry whose derived state cannot be trusted, for `reason`.
    pub const fn untrusted(reason: UntrustedReason) -> Self {
        TrustState::Untrusted { reason }
    }
}

/// The kind of work an entry is doing while it warms.
///
/// On the wire a phase is the flat string itself: `"installing_coverage"`,
/// `"healing"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WarmingPhase {
    /// The entry is acquiring everything a read runs on, before it reads
    /// anything: the vault's own working area, sole maintainership of it,
    /// change detection over the vault tree, and the derived state reads
    /// answer from. All of it precedes the first document, so the counters
    /// beside this phase stand at zero against an unknown total, and an entry
    /// may hold here for a while on a loaded machine without anything being
    /// wrong.
    InstallingCoverage,
    /// Documents are being derived, and the counters beside this phase advance
    /// as they are.
    Healing,
}

/// Why an entry's derived state cannot be trusted.
///
/// On the wire a reason is an object tagged `kind`:
/// `{"kind":"watcher_overflow"}`.
///
/// A refusal carries the same reason as its typed detail, so a caller reads
/// one vocabulary whether it polled the state or was refused by it.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum UntrustedReason {
    /// The watcher is covering the vault and reported less than it saw — an
    /// overflow, or notifications it dropped — so what changed while it was
    /// not reporting is unknown. The entry re-heals from this on its own.
    WatcherOverflow,
    /// Watcher coverage ended, so no change to the vault is reported from here
    /// on. The entry re-heals when a client asks it to.
    #[non_exhaustive]
    WatcherLost {
        /// What ended coverage.
        cause: WatcherLossCause,
        /// The loss in words, for a person reading a message or a log.
        /// Clients never match on it: `cause` is what a client branches on.
        detail: String,
    },
    /// The environment refused the work: the disk is full, or a vault path
    /// stopped being readable. The stored state is sound and the environment
    /// is not.
    #[non_exhaustive]
    EnvironmentalRefusal {
        /// The refusal in words, for a person reading a message or a log.
        /// Clients never match on it: the reason's `kind` and the refusal's
        /// code are what a client branches on.
        detail: String,
    },
}

impl UntrustedReason {
    /// Watcher coverage ended, for `cause`, described by `detail`.
    pub fn watcher_lost(cause: WatcherLossCause, detail: impl Into<String>) -> Self {
        UntrustedReason::WatcherLost {
            cause,
            detail: detail.into(),
        }
    }

    /// The environment refused the work, described by `detail`.
    pub fn environmental_refusal(detail: impl Into<String>) -> Self {
        UntrustedReason::EnvironmentalRefusal {
            detail: detail.into(),
        }
    }
}

/// What ended watcher coverage for an entry.
///
/// On the wire a cause is the flat string itself: `"backend"`,
/// `"coverage_lost"`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WatcherLossCause {
    /// The watcher backend stopped: it could not be installed over the vault,
    /// or it failed while running.
    Backend,
    /// The vault root the watcher covered stopped being covered — it was
    /// removed, replaced, or moved out from under the watch.
    CoverageLost,
}
