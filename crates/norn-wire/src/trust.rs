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
//! reaches readable state through work of two kinds — acquiring what a read
//! runs on, then deriving documents — and only the second of them has anything
//! to count. Counters alone therefore cannot tell a heal that has not started
//! from one that is not progressing: both read as zero healed against an
//! unknown total. [`WarmingPhase`] is what separates them, so a caller waiting
//! on an entry, and an operator reading why a wait expired, learn which of the
//! two the entry is in. A third phase names the leg that runs the other way,
//! giving those resources back; it counts nothing either, and it is the reason
//! [`TrustState::Warming`] is read as "attached and not readable" rather than
//! as a promise about where the entry is heading.
//!
//! **Two watcher reasons, split by who resumes.** [`UntrustedReason::WatcherOverflow`]
//! is a running watcher that reported less than it saw, and the entry re-heals
//! itself; [`UntrustedReason::WatcherLost`] is coverage that ended, and the
//! entry re-heals when a client demands it. One word for both would make a
//! state that recovers on its own indistinguishable from one that waits.
//!
//! **A broken environment and damaged derived state are two reasons for the
//! same shape of reason.** [`UntrustedReason::EnvironmentalRefusal`] says the
//! stored state is sound and the environment is not, so the resolution is the
//! environment being fixed and the work being tried again. A damage verdict
//! says the opposite: the vault is sound and the derivation over it is not, so
//! trying the same work again resolves nothing and the entry discards its
//! derived state and builds it from the vault instead. One word for both would
//! publish a state that resumes by itself as one that waits on a machine
//! somebody has to repair.
//!
//! **Two damage reasons, split by who resumes**, on the same principle the
//! watcher pair is split on. [`UntrustedReason::StoreDamagedRebuilding`] is an
//! entry holding the damaged derived state: the rung that discards it runs
//! against what the entry has, so the entry resumes on its own.
//! [`UntrustedReason::StoreDamagedAwaitingDemand`] is an entry holding nothing
//! — the run that met the damage was the one establishing the entry, and it
//! could not resolve what it met — so there is no database to discard until a
//! demand opens one, and the entry waits for that demand. The distinction was
//! once documented as a read of what the entry is holding, which no client can
//! perform: holdings cross no seam, and a fact a client branches on is a tag
//! here or it is nothing.
//!
//! **A run that ended in a panic is a word of its own.**
//! [`UntrustedReason::LegUnwound`] says the work over the vault stopped part
//! way through and reached no verdict about anything. The two verdicts beside
//! it each say something a run established — the environment refused, or the
//! derived state is damaged — and an unwind establishes neither: what the run
//! was doing is unfinished, and what it was holding is given back unread.
//! Spelling it as either would publish a diagnosis nothing made.

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
    /// Registered and holding nothing. There is no derived state to read from
    /// yet, and none out on loan either: change detection over the vault, the
    /// derived state and sole maintainership of that derived state have all
    /// been given back before an entry publishes this. A caller waiting for it
    /// is waiting for exactly that, on every entry and every path that reaches
    /// it.
    Unattached,
    /// Attached and not readable. The entry is either working toward readable
    /// derived state or giving up what it holds; `phase` says which, and the
    /// counters say how far the counted work has come. Reads do not answer
    /// from the entry in either case.
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

    /// What this state refuses a request with, or `None` where the state
    /// itself is what the request is answered with.
    ///
    /// A state the entry walks out of on its own is answered with: `Warming`
    /// and `Unattached` are polled — a caller reads how far the entry has come
    /// and asks again — and `Ready` is read from. Reads answer from none of the
    /// first two, and that is not what decides the question: a state that
    /// stands until something other than that polling retires it — a re-heal,
    /// a client demanding one, an environment that stops refusing — is a
    /// refusal, and it carries the reason it stands on.
    ///
    /// A state minted beside these takes its stance here. The match carries no
    /// wildcard, so the question is answered where the states are written
    /// rather than fallen through at whichever caller reads one.
    pub const fn refusal(&self) -> Option<&UntrustedReason> {
        match self {
            TrustState::Unattached | TrustState::Warming { .. } | TrustState::Ready => None,
            TrustState::Untrusted { reason } => Some(reason),
        }
    }
}

/// The kind of work an entry is doing while it is attached and not readable.
///
/// On the wire a phase is the flat string itself: `"installing_coverage"`,
/// `"healing"`, `"releasing_coverage"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WarmingPhase {
    /// The entry is acquiring everything a read runs on, before it reads
    /// anything: the vault's own working area, the derived state reads answer
    /// from, sole maintainership of that derived state, and change detection
    /// over the vault tree. All of it precedes the first document, so the counters
    /// beside this phase stand at zero against an unknown total, and an entry
    /// may hold here for a while on a loaded machine without anything being
    /// wrong.
    InstallingCoverage,
    /// Documents are being derived, and the counters beside this phase advance
    /// as they are.
    Healing,
    /// The entry is giving back everything [`WarmingPhase::InstallingCoverage`]
    /// acquired: change detection over the vault ends, the derived state is
    /// closed, and sole maintainership of it is given up — released
    /// where the entry still holds it, and put down where the entry has already
    /// lost it to another maintainer. Nothing is counted here, so the counters
    /// beside this phase stand at zero against an unknown total.
    ///
    /// This phase is the one that does not end at [`TrustState::Ready`]: it
    /// ends at [`TrustState::Unattached`], and an entry only reaches
    /// [`TrustState::Ready`] from here by being demanded again afterwards. It
    /// sits under [`TrustState::Warming`] because that is the state meaning
    /// "attached and not readable", which is what the entry is while its
    /// resources are still being released.
    ///
    /// A demand arriving now neither blocks nor fails: the release is what
    /// answers it. Where the entry is free to acquire the resources again, the
    /// release honors the demand by doing so once it finishes. Where the entry
    /// stands parked — another process holding the vault's maintainer lock, the
    /// registered root reached under a second name, or that root refused on its
    /// identity — the release answers with the park instead, because
    /// re-acquiring is not something the entry is allowed to do while any of
    /// them stands.
    ReleasingCoverage,
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
    /// The derived state is damaged: the database is not one this build wrote,
    /// its pages are corrupt, or it holds values no reader will accept. The
    /// vault is sound and the derivation over it is not, so the entry discards
    /// the database and derives it again from the vault. The entry holds that
    /// database and rebuilds it on its own.
    #[non_exhaustive]
    StoreDamagedRebuilding {
        /// The damage in words, for a person reading a message or a log.
        /// Clients never match on it: the reason's `kind` is what a client
        /// branches on.
        detail: String,
    },
    /// The derived state is damaged, and the run that met the damage was the
    /// one establishing the entry, so the entry holds no database to discard.
    /// The vault is sound and the derivation over it is not; the entry derives
    /// again when a client demands it, and that demand is what opens a database
    /// to discard.
    #[non_exhaustive]
    StoreDamagedAwaitingDemand {
        /// The damage in words, for a person reading a message or a log.
        /// Clients never match on it: the reason's `kind` is what a client
        /// branches on.
        detail: String,
    },
    /// The run of work over this vault ended in a panic. It reached no verdict
    /// about the vault or about the derived state, so neither is said to be at
    /// fault; what it was holding is given back unread. The entry holds nothing
    /// from here and is served again when a client demands it.
    #[non_exhaustive]
    LegUnwound {
        /// The panic in words, for a person reading a message or a log.
        /// Clients never match on it: the reason's `kind` is what a client
        /// branches on.
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

    /// The derived state the entry holds is damaged and is being rebuilt from
    /// the vault, described by `detail`.
    pub fn store_damaged_rebuilding(detail: impl Into<String>) -> Self {
        UntrustedReason::StoreDamagedRebuilding {
            detail: detail.into(),
        }
    }

    /// The derived state is damaged, the entry holds none of it, and a demand
    /// is what establishes the database to discard, described by `detail`.
    pub fn store_damaged_awaiting_demand(detail: impl Into<String>) -> Self {
        UntrustedReason::StoreDamagedAwaitingDemand {
            detail: detail.into(),
        }
    }

    /// The run of work over the vault ended in a panic, described by `detail`.
    pub fn leg_unwound(detail: impl Into<String>) -> Self {
        UntrustedReason::LegUnwound {
            detail: detail.into(),
        }
    }
}

/// What ended watcher coverage for an entry.
///
/// On the wire a cause is the flat string itself: `"backend"`,
/// `"coverage_lost"`, or `"synchronization_expired"`.
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
    /// Coverage did not cross its backend synchronization boundary before the
    /// authored lifecycle deadline.
    SynchronizationExpired,
}
