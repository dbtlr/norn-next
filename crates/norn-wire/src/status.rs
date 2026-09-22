//! Where a registration stands, and what a roll-up over every one of them
//! says.
//!
//! **A registration on the wire is the registry entry, field for field.** The
//! four fields a registry entry holds are the four fields a `vault register`
//! carries in, a `vault list` reports, and a `vault set` reports back after an
//! edit. One shape rather than three keeps a registration read the same way
//! wherever it is met, and the config crate converts its own entry to and from
//! this without a field falling out on either trip.
//!
//! **A status reports the published demand whole.** What the host publishes
//! for an entry is either a trust state or a refusal, and [`Published`] is
//! that pair rather than a state with a refusal beside it: a parked entry
//! reports its park under the park's own code, and an unparked entry reports
//! where its derived state stands. Flattening the two into one state would
//! make a park indistinguishable from an entry that happens to be untrusted,
//! which is the one distinction an operator acts on.
//!
//! **A status creates no demand.** Every reading here is taken off what the
//! entry already publishes and off the control files as they are authored, so
//! asking where a vault stands never attaches one, never warms one, and never
//! moves an entry that was about to be detached.
//!
//! **The authoring fact and the slot fact are two readings.** An
//! [`EngineStatus`] says what the vault's engine slot is doing now; the
//! [`EngineSection`] beside it says what the host was delivered as that
//! vault's engine section. A vault whose config enables an engine that is not
//! standing is those two readings disagreeing, and a status that carried only
//! one of them could not say so.
//!
//! **A roll-up is derived from the statuses it rolls up.** Its counts and the
//! attention it names are computed from a list of [`VaultStatus`], so nothing
//! can hand across a roll-up whose counts disagree with the vaults beside it.
//! Every entry falls in exactly one of the five counts, which is what makes
//! them sum to the vaults counted.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::address::{PollBackend, SchemaSource, VaultRoot};
use crate::error::{ErrorEnvelope, ReasonCode};
use crate::name::VaultName;
use crate::reading::{EngineSection, Freshness};
use crate::reload::ReloadFailure;
use crate::trust::{TrustState, UntrustedReason};

/// One registered vault, as the registry holds it.
///
/// Four fields and no others: anything a host learns by looking at the vault
/// is derived state and is reported beside a registration rather than in it.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Registration {
    /// The name the vault is addressed and keyed by.
    pub name: VaultName,
    /// The vault's root directory.
    pub root: VaultRoot,
    /// Where the vault's schema is read from. `null` is the in-vault default,
    /// relative to the root.
    pub schema_source: Option<SchemaSource>,
    /// The watch backend this registration pins. `null` is the platform's
    /// native one.
    pub poll_backend: Option<PollBackend>,
}

impl Registration {
    /// The vault `name` rooted at `root`, reading its schema from the in-vault
    /// default and watched by the platform's native backend.
    pub const fn new(name: VaultName, root: VaultRoot) -> Self {
        Registration {
            name,
            root,
            schema_source: None,
            poll_backend: None,
        }
    }

    /// The registration reading its schema from `schema_source`.
    #[must_use]
    pub fn with_schema_source(mut self, schema_source: SchemaSource) -> Self {
        self.schema_source = Some(schema_source);
        self
    }

    /// The registration watched through `poll_backend`.
    #[must_use]
    pub const fn with_poll_backend(mut self, poll_backend: PollBackend) -> Self {
        self.poll_backend = Some(poll_backend);
        self
    }
}

/// What the host publishes for one entry.
///
/// On the wire a published answer is an object tagged `answer`:
/// `{"answer":"state","state":{"state":"ready"}}`,
/// `{"answer":"parked","refusal":{…}}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Published {
    /// The entry publishes where its derived state stands.
    #[non_exhaustive]
    State {
        /// Where the entry stands.
        state: TrustState,
    },
    /// The entry publishes a refusal: every request against it is answered
    /// with this, under the park's own code.
    #[non_exhaustive]
    Parked {
        /// The refusal the park publishes.
        refusal: ErrorEnvelope,
    },
}

impl Published {
    /// The entry publishes the trust `state`.
    pub const fn state(state: TrustState) -> Self {
        Published::State { state }
    }

    /// The entry publishes the `refusal` it is parked under.
    pub const fn parked(refusal: ErrorEnvelope) -> Self {
        Published::Parked { refusal }
    }

    /// The published demand as an answer for the entry renders it: a trust
    /// state where the entry answers with one, and the park's own refusal
    /// where it does not.
    pub fn of(answer: Result<TrustState, ErrorEnvelope>) -> Self {
        match answer {
            Ok(state) => Published::state(state),
            Err(refusal) => Published::parked(refusal),
        }
    }
}

/// The control-file fingerprints a vault is serving under.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Fingerprints {
    /// The active vault schema's fingerprint.
    pub schema: String,
    /// The active vault config's fingerprint, and `null` where no config file
    /// is there at all and the vault serves the missing-file default.
    pub config: Option<String>,
}

impl Fingerprints {
    /// A vault serving `schema`, with no config file behind it.
    pub fn new(schema: impl Into<String>) -> Self {
        Fingerprints {
            schema: schema.into(),
            config: None,
        }
    }

    /// The fingerprints with `config` read from a config file that is there.
    #[must_use]
    pub fn with_config(mut self, config: impl Into<String>) -> Self {
        self.config = Some(config.into());
        self
    }
}

/// Where the authored control files stand against the ones the vault is
/// serving.
///
/// On the wire a drift is an object tagged `state`:
/// `{"state":"current"}`, `{"state":"unreadable","failure":{…}}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Drift {
    /// The entry is serving no control files yet, so there is nothing for the
    /// authored ones to have drifted from.
    Inactive {},
    /// The authored control files are the ones the vault is serving.
    Current {},
    /// The authored control files differ from the ones the vault is serving.
    /// No watcher event activates a control file, so a reload is what applies
    /// them.
    ReloadPending {},
    /// The authored control files could not be read, so whether they have
    /// drifted is not known.
    #[non_exhaustive]
    Unreadable {
        /// What refused the reading.
        failure: ReloadFailure,
    },
}

impl Drift {
    /// The entry is serving no control files yet.
    pub const fn inactive() -> Self {
        Drift::Inactive {}
    }

    /// The authored control files are the active ones.
    pub const fn current() -> Self {
        Drift::Current {}
    }

    /// The authored control files differ from the active ones.
    pub const fn reload_pending() -> Self {
        Drift::ReloadPending {}
    }

    /// The authored control files could not be read, for `failure`.
    pub const fn unreadable(failure: ReloadFailure) -> Self {
        Drift::Unreadable { failure }
    }
}

/// What a vault's engine slot is doing.
///
/// On the wire an engine status is an object tagged `state`:
/// `{"state":"off"}`,
/// `{"state":"on","last_drain_error":null,"freshness":null}`,
/// `{"state":"self_disabled","detail":"…"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum EngineStatus {
    /// No engine stands for this vault here and now: the delivered section
    /// enables none, or the vault is not attached.
    Off {},
    /// An engine stands for this vault.
    #[non_exhaustive]
    On {
        /// What the last drain refused with, in words, for a person reading a
        /// message or a log. `null` where the last drain did not refuse.
        /// Clients never match on it.
        last_drain_error: Option<String>,
        /// How far the engine's derived state trails the store, and `null`
        /// until the engine reports a watermark.
        freshness: Option<Freshness>,
    },
    /// The engine took itself out of service.
    #[non_exhaustive]
    SelfDisabled {
        /// Why it did, in words, for a person reading a message or a log.
        /// Clients never match on it.
        detail: String,
    },
}

impl EngineStatus {
    /// No engine stands for this vault.
    pub const fn off() -> Self {
        EngineStatus::Off {}
    }

    /// An engine stands, whose last drain refused with `last_drain_error` —
    /// `None` where it did not — and whose derived state is at `freshness`,
    /// `None` until the engine reports a watermark.
    pub const fn on(last_drain_error: Option<String>, freshness: Option<Freshness>) -> Self {
        EngineStatus::On {
            last_drain_error,
            freshness,
        }
    }

    /// The engine took itself out of service, for `detail`.
    pub fn self_disabled(detail: impl Into<String>) -> Self {
        EngineStatus::SelfDisabled {
            detail: detail.into(),
        }
    }
}

/// Something about a vault's serving worth telling an operator, which is not a
/// refusal and does not move its trust state.
///
/// On the wire an advisory is an object tagged `kind`:
/// `{"kind":"tmp_fallback_in_use","path":"…","gitignored":true}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Advisory {
    /// The vault-local `.norn/tmp` shadow home is in use, because the derived
    /// directory is on another filesystem and a rename cannot cross one. The
    /// directory is inside the vault, so whether the vault ignores it is a
    /// fact an operator acts on.
    #[non_exhaustive]
    TmpFallbackInUse {
        /// The shadow home's directory.
        path: String,
        /// Whether the vault ignores that directory.
        gitignored: bool,
    },
    /// A symbolic link under the vault root was not walked.
    #[non_exhaustive]
    SymlinkSkipped {
        /// The link's path.
        path: String,
    },
}

impl Advisory {
    /// The vault-local shadow home at `path` is in use, ignored by the vault
    /// or not.
    pub fn tmp_fallback_in_use(path: impl Into<String>, gitignored: bool) -> Self {
        Advisory::TmpFallbackInUse {
            path: path.into(),
            gitignored,
        }
    }

    /// The symbolic link at `path` was not walked.
    pub fn symlink_skipped(path: impl Into<String>) -> Self {
        Advisory::SymlinkSkipped { path: path.into() }
    }
}

/// One vault a roll-up names as wanting attention, and what about it.
///
/// On the wire an attention reason is an object tagged `attention`:
/// `{"attention":"reload_pending","name":"notes"}`. The tag is not `reason`,
/// because the untrusted reason a member of this carries is spelled `reason`
/// wherever it is met, and an internally tagged enum cannot hold a field whose
/// name is its own tag.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "attention", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Attention {
    /// The vault's derived state cannot be trusted.
    #[non_exhaustive]
    Untrusted {
        /// The vault.
        name: VaultName,
        /// What made the state untrustworthy.
        reason: UntrustedReason,
    },
    /// The vault's entry is parked: every request against it is refused.
    #[non_exhaustive]
    Parked {
        /// The vault.
        name: VaultName,
        /// The code the park refuses under.
        code: ReasonCode,
    },
    /// The vault is served and its reads refuse, because its coverage minted
    /// no read handle.
    #[non_exhaustive]
    ReadsRefusing {
        /// The vault.
        name: VaultName,
        /// Why they refuse, in words, for a person reading a message or a log.
        /// Clients never match on it.
        detail: String,
    },
    /// The vault's last reload did not leave it serving what its control files
    /// state.
    #[non_exhaustive]
    ReloadFailed {
        /// The vault.
        name: VaultName,
        /// What the reload met.
        failure: ReloadFailure,
    },
    /// The vault's authored control files differ from the ones it is serving.
    #[non_exhaustive]
    ReloadPending {
        /// The vault.
        name: VaultName,
    },
    /// The vault's engine took itself out of service.
    #[non_exhaustive]
    EngineSelfDisabled {
        /// The vault.
        name: VaultName,
        /// Why it did, in words, for a person reading a message or a log.
        /// Clients never match on it.
        detail: String,
    },
    /// The vault carries an advisory.
    #[non_exhaustive]
    Advisory {
        /// The vault.
        name: VaultName,
        /// What the advisory says.
        advisory: Advisory,
    },
}

impl Attention {
    /// The vault `name` cannot be trusted, for `reason`.
    pub const fn untrusted(name: VaultName, reason: UntrustedReason) -> Self {
        Attention::Untrusted { name, reason }
    }

    /// The vault `name` is parked under `code`.
    pub const fn parked(name: VaultName, code: ReasonCode) -> Self {
        Attention::Parked { name, code }
    }

    /// The vault `name` is served and its reads refuse, for `detail`.
    pub fn reads_refusing(name: VaultName, detail: impl Into<String>) -> Self {
        Attention::ReadsRefusing {
            name,
            detail: detail.into(),
        }
    }

    /// The vault `name`'s last reload met `failure`.
    pub const fn reload_failed(name: VaultName, failure: ReloadFailure) -> Self {
        Attention::ReloadFailed { name, failure }
    }

    /// The vault `name` has control files it is not serving.
    pub const fn reload_pending(name: VaultName) -> Self {
        Attention::ReloadPending { name }
    }

    /// The vault `name`'s engine took itself out of service, for `detail`.
    pub fn engine_self_disabled(name: VaultName, detail: impl Into<String>) -> Self {
        Attention::EngineSelfDisabled {
            name,
            detail: detail.into(),
        }
    }

    /// The vault `name` carries `advisory`.
    pub const fn advisory(name: VaultName, advisory: Advisory) -> Self {
        Attention::Advisory { name, advisory }
    }
}

/// Where one vault's entry stands and what it is serving from.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct VaultStatus {
    /// The vault this is the standing of.
    pub name: VaultName,
    /// The registration behind it.
    pub registration: Registration,
    /// What the entry publishes: where it stands, or the refusal it is parked
    /// under.
    pub published: Published,
    /// The control-file fingerprints it is serving under, and `null` where it
    /// is serving none yet.
    pub fingerprints: Option<Fingerprints>,
    /// Where the authored control files stand against the ones it is serving.
    pub drift: Drift,
    /// What the last reload met, and `null` where no reload has failed.
    pub last_reload_failure: Option<ReloadFailure>,
    /// Why this entry's reads refuse, in words, and `null` where they do not.
    /// An entry whose read seam is down is still serving every other surface,
    /// so this moves no trust state. Clients never match on it.
    pub reads_refusing: Option<String>,
    /// What the vault's engine slot is doing.
    pub engine: EngineStatus,
    /// What the host was delivered as this vault's engine section. The
    /// authoring fact beside the slot fact above: a config that enables an
    /// engine which is not standing is the two disagreeing.
    pub section: EngineSection,
    /// What is worth telling an operator about this vault's serving. Empty
    /// where there is nothing.
    pub advisories: Vec<Advisory>,
}

impl VaultStatus {
    /// The standing of the vault `registration` names: what it publishes,
    /// where its control files have drifted, and what its engine is doing.
    /// Nothing has failed a reload, its reads are not refusing, it is serving
    /// no fingerprints, and it carries no advisory.
    pub fn new(
        registration: Registration,
        published: Published,
        drift: Drift,
        engine: EngineStatus,
        section: EngineSection,
    ) -> Self {
        VaultStatus {
            name: registration.name.clone(),
            registration,
            published,
            fingerprints: None,
            drift,
            last_reload_failure: None,
            reads_refusing: None,
            engine,
            section,
            advisories: Vec::new(),
        }
    }

    /// The status serving under `fingerprints`.
    #[must_use]
    pub fn with_fingerprints(mut self, fingerprints: Fingerprints) -> Self {
        self.fingerprints = Some(fingerprints);
        self
    }

    /// The status whose last reload met `failure`.
    #[must_use]
    pub fn with_last_reload_failure(mut self, failure: ReloadFailure) -> Self {
        self.last_reload_failure = Some(failure);
        self
    }

    /// The status whose reads refuse, for `detail`.
    #[must_use]
    pub fn with_reads_refusing(mut self, detail: impl Into<String>) -> Self {
        self.reads_refusing = Some(detail.into());
        self
    }

    /// The status carrying `advisories`.
    #[must_use]
    pub fn with_advisories(mut self, advisories: impl IntoIterator<Item = Advisory>) -> Self {
        self.advisories = advisories.into_iter().collect();
        self
    }

    /// What this entry counts toward in a roll-up.
    ///
    /// Every entry falls in exactly one count: a park is what it publishes
    /// whatever its derived state is doing underneath, and an unparked entry
    /// counts by the state it publishes. The match carries no wildcard, so a
    /// published answer or a trust state minted without a count does not
    /// compile.
    const fn counts_as(&self) -> Count {
        match &self.published {
            Published::Parked { .. } => Count::Parked,
            Published::State { state } => match state {
                TrustState::Unattached => Count::Unattached,
                TrustState::Warming { .. } => Count::Warming,
                TrustState::Ready => Count::Ready,
                TrustState::Untrusted { .. } => Count::Untrusted,
            },
        }
    }

    /// Everything about this entry a roll-up names as wanting attention.
    fn attention(&self) -> Vec<Attention> {
        let mut attention = Vec::new();
        match &self.published {
            Published::Parked { refusal } => {
                attention.push(Attention::parked(self.name.clone(), refusal.code().clone()));
            }
            Published::State {
                state: TrustState::Untrusted { reason },
            } => {
                attention.push(Attention::untrusted(self.name.clone(), reason.clone()));
            }
            Published::State { .. } => {}
        }
        if let Some(detail) = &self.reads_refusing {
            attention.push(Attention::reads_refusing(self.name.clone(), detail));
        }
        if let Some(failure) = &self.last_reload_failure {
            attention.push(Attention::reload_failed(self.name.clone(), failure.clone()));
        }
        if matches!(self.drift, Drift::ReloadPending {}) {
            attention.push(Attention::reload_pending(self.name.clone()));
        }
        if let EngineStatus::SelfDisabled { detail } = &self.engine {
            attention.push(Attention::engine_self_disabled(self.name.clone(), detail));
        }
        attention.extend(
            self.advisories
                .iter()
                .map(|advisory| Attention::advisory(self.name.clone(), advisory.clone())),
        );
        attention
    }
}

/// Which of a roll-up's five counts one entry falls in.
///
/// Not a wire type: it exists so the classification is written once and the
/// counts are read off it, rather than five predicates that could overlap or
/// leave a gap.
enum Count {
    Ready,
    Warming,
    Untrusted,
    Parked,
    Unattached,
}

/// What every entry this installation serves adds up to.
///
/// The five counts partition the vaults counted, so they sum to `vaults`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct RollUp {
    /// How many vaults were counted.
    pub vaults: u64,
    /// How many are ready: their derived state is current and reads answer
    /// from it.
    pub ready: u64,
    /// How many are warming: attached, and not readable yet.
    pub warming: u64,
    /// How many are untrusted: attached, and their derived state cannot be
    /// trusted.
    pub untrusted: u64,
    /// How many are parked: every request against them is refused.
    pub parked: u64,
    /// How many are unattached: registered and holding nothing.
    pub unattached: u64,
    /// Every vault that wants attention, and what about it. A vault wanting
    /// two things is named twice.
    pub attention: Vec<Attention>,
}

impl RollUp {
    /// The roll-up `statuses` add up to.
    ///
    /// The counts and the attention are derived here rather than passed in, so
    /// a roll-up cannot disagree with the statuses it rolls up.
    pub fn of(statuses: &[VaultStatus]) -> Self {
        let mut roll_up = RollUp {
            vaults: statuses.len() as u64,
            ready: 0,
            warming: 0,
            untrusted: 0,
            parked: 0,
            unattached: 0,
            attention: Vec::new(),
        };
        for status in statuses {
            match status.counts_as() {
                Count::Ready => roll_up.ready += 1,
                Count::Warming => roll_up.warming += 1,
                Count::Untrusted => roll_up.untrusted += 1,
                Count::Parked => roll_up.parked += 1,
                Count::Unattached => roll_up.unattached += 1,
            }
            roll_up.attention.extend(status.attention());
        }
        roll_up
    }
}
