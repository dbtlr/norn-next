//! The one structured error envelope: a reason code, a message, and the typed
//! detail the code carries.
//!
//! **There is deliberately no `retryable` flag.** Whether an operation is
//! worth trying again follows from the code and the detail, and a boolean
//! beside them is a second answer to that question that can disagree with the
//! first.
//!
//! **There is deliberately no forecast field.** Riding a forecast on every
//! refusal would make every refusal carry a shape only some refusals have.
//!
//! Both absences follow one placement rule: **a payload that belongs to one
//! refusal belongs to that code's [`ErrorDetail`] variant, never to a field on
//! the envelope.** An envelope field is carried by every refusal, so a field
//! only one code fills is a field every other code leaves empty, and readers
//! learn to check the code before believing it.
//!
//! **Two codes here are minted ahead of the layer that produces them.**
//! `vault/ambiguous-root` answers a `vault resolve` ask over a directory that
//! more than one registration contains; `host/registry-unwritable` answers a
//! registration change whose write of the registry file refused. Both are the
//! vault-namespace handlers' to raise, and those handlers land above this
//! layer (NORN-231), so the call graph reaches neither from this crate today.
//! They are spelled here because the vocabulary a refusal is spelled in is not
//! a surface's to choose: the handler that arrives renders one of these rather
//! than minting a string of its own.
//!
//! The pairing between a code and its detail is structural rather than a rule
//! constructors keep: [`ErrorEnvelope::new`] takes the code from the detail,
//! and the read path refuses an envelope whose code is not the code its detail
//! carries. `ReasonCode` is matched without a wildcard in this module's tests,
//! so a code minted without its detail does not compile.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::cursor::CursorOrderChanged;
use crate::demand::AttachMode;
use crate::finding_row::{CandidateHead, Hint};
use crate::name::VaultName;
use crate::reading::Rung;
use crate::reload::ReloadFailure;
use crate::target::ResolutionTarget;
use crate::trust::{NotReady, UntrustedReason};

/// Who holds a contended maintainer lock, as far as its diagnostic says.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum MaintainerIdentity {
    /// The lock diagnostic identified its holder.
    #[non_exhaustive]
    Named {
        /// The process id reported by the holder.
        pid: u32,
        /// The Norn version reported by the holder.
        version: String,
        /// Whole seconds since the Unix epoch when the holder took the lock.
        started_unix_seconds: u64,
    },
    /// The lock is held, but its diagnostic could not identify the holder.
    Unknown {},
}

impl MaintainerIdentity {
    /// An identified maintainer.
    pub fn named(pid: u32, version: impl Into<String>, started_unix_seconds: u64) -> Self {
        Self::Named {
            pid,
            version: version.into(),
            started_unix_seconds,
        }
    }

    /// A maintainer whose diagnostic identity is unavailable.
    pub const fn unknown() -> Self {
        Self::Unknown {}
    }
}

/// The code a refusal is filed under.
///
/// A code is a flat namespaced string — `namespace/what-happened` — and the
/// list holds every code the system can emit today.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub enum ReasonCode {
    /// `host/duplicate-root` — more than one registered name resolves to this
    /// vault's root, so none of them is served until the registry names one.
    /// The detail is every name that resolves to it.
    #[serde(rename = "host/duplicate-root")]
    HostDuplicateRoot,
    /// `host/entry-untrusted` — the vault entry's derived state cannot be
    /// trusted, so the request is refused rather than answered. The detail is
    /// the reason the state is untrusted.
    #[serde(rename = "host/entry-untrusted")]
    HostEntryUntrusted,
    /// `host/maintainer-contended` — another process currently maintains this
    /// vault's derived state.
    #[serde(rename = "host/maintainer-contended")]
    HostMaintainerContended,
    /// `host/unknown-vault` — the requested name is not in the registry, so
    /// there is no vault to serve under it. The detail is the name that was
    /// asked for.
    #[serde(rename = "host/unknown-vault")]
    HostUnknownVault,
    /// `host/unsupported-attach-mode` — the demand named a mode the host holds
    /// no lifecycle for, so nothing was read or written under it. The detail is
    /// the mode that was named.
    #[serde(rename = "host/unsupported-attach-mode")]
    HostUnsupportedAttachMode,
    /// `host/already-served` — the host already serves an entry under this
    /// name, so nothing was registered over it. The detail is the name.
    #[serde(rename = "host/already-served")]
    HostAlreadyServed,
    /// `host/entry-held` — the entry is holding something, or something is
    /// holding it, so it was not taken out of service. The detail is the name.
    #[serde(rename = "host/entry-held")]
    HostEntryHeld,
    /// `host/entry-not-ready` — the entry holds nothing the request can be
    /// answered from yet. The detail is where the entry stands.
    #[serde(rename = "host/entry-not-ready")]
    HostEntryNotReady,
    /// `host/reader-unavailable` — the entry is serving and its read seam is
    /// not. The detail is the account the act that failed produced.
    #[serde(rename = "host/reader-unavailable")]
    HostReaderUnavailable,
    /// `host/registry-unwritable` — the registry file could not be written,
    /// so the registration change was not made and the registration that
    /// stood before it still stands. The detail is the write's own account of
    /// what refused.
    #[serde(rename = "host/registry-unwritable")]
    HostRegistryUnwritable,
    /// `vault/ambiguous-root` — the directory that was asked about is
    /// contained by more than one registration, so the ask names no one vault.
    /// It answers a resolution of a directory and never a request against an
    /// entry: what a host refuses about entries it serves under names that
    /// resolve to one root is `host/duplicate-root`. The detail is every name
    /// that contains the directory.
    #[serde(rename = "vault/ambiguous-root")]
    VaultAmbiguousRoot,
    /// `vault/ambiguous-target` — the target the request named resolves to
    /// more than one document, so there is no one document to answer about.
    /// The detail is the target, the bounded head of what it resolves to, and
    /// what to ask to see the whole class.
    #[serde(rename = "vault/ambiguous-target")]
    VaultAmbiguousTarget,
    /// `vault/unknown-target` — the target the request named resolves to no
    /// document in this vault. The detail is the target.
    #[serde(rename = "vault/unknown-target")]
    VaultUnknownTarget,
    /// `vault/reload-busy` — the vault is serving and something is already
    /// working over it, so the reload was not started. The detail carries
    /// nothing: the ask is repeated rather than resolved.
    #[serde(rename = "vault/reload-busy")]
    VaultReloadBusy,
    /// `vault/reload-failed` — the reload ran and did not leave the vault
    /// serving what its control files state. The detail is what it met.
    #[serde(rename = "vault/reload-failed")]
    VaultReloadFailed,
    /// `vault/cursor-order-changed` — the cursor was minted under one order
    /// and continued under another, so the position it names is in a sequence
    /// that no longer exists. The detail is the two orders.
    #[serde(rename = "vault/cursor-order-changed")]
    VaultCursorOrderChanged,
    /// `engine/not-enabled` — the vault has not enabled the rung the request
    /// asked for. The detail is the rung, and what to do about it.
    #[serde(rename = "engine/not-enabled")]
    EngineNotEnabled,
    /// `engine/unavailable` — the rung is enabled and no engine stands for it
    /// here and now. The detail is the rung and why.
    #[serde(rename = "engine/unavailable")]
    EngineUnavailable,
    /// `engine/failed` — the engine stands for the rung and this answer
    /// failed. The detail is the rung and the failure.
    #[serde(rename = "engine/failed")]
    EngineFailed,
}

/// The typed payload one reason code carries.
///
/// One detail shape per code, and the detail's `code` tag *is* the code:
/// `{"code":"host/entry-untrusted","reason":{"kind":"watcher_overflow"}}`.
///
/// A detail composes the types the rest of the vocabulary already uses — an
/// untrusted entry's detail is the same reason its trust state carries.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "code")]
#[non_exhaustive]
pub enum ErrorDetail {
    /// The detail of `host/duplicate-root`: every registered name that
    /// resolves to the one vault root.
    #[serde(rename = "host/duplicate-root")]
    #[non_exhaustive]
    DuplicateRoot {
        /// The colliding names, in ascending order, each echoed back as the
        /// typed name: the registry holds them as names parsed through the
        /// grammar, and the refusal hands the same parsed values back rather
        /// than strings a reader would have to parse again.
        aliases: Vec<VaultName>,
    },
    /// The detail of `host/entry-untrusted`: why the entry's derived state
    /// cannot be trusted.
    #[serde(rename = "host/entry-untrusted")]
    #[non_exhaustive]
    EntryUntrusted {
        /// Why the entry's derived state cannot be trusted.
        reason: UntrustedReason,
    },
    /// The detail of `host/maintainer-contended`: who holds maintainership, as
    /// far as the lock diagnostic can say.
    #[serde(rename = "host/maintainer-contended")]
    #[non_exhaustive]
    MaintainerContended {
        /// The incumbent maintainer's diagnostic identity.
        incumbent: MaintainerIdentity,
    },
    /// The detail of `host/unknown-vault`: the name the request asked for.
    #[serde(rename = "host/unknown-vault")]
    #[non_exhaustive]
    UnknownVault {
        /// The vault name the request asked for, echoed back as the typed
        /// name: the request named it through the grammar, and the refusal
        /// hands the same parsed value back rather than a string a reader
        /// would have to parse again.
        name: VaultName,
    },
    /// The detail of `host/unsupported-attach-mode`: the mode the demand
    /// named.
    #[serde(rename = "host/unsupported-attach-mode")]
    #[non_exhaustive]
    UnsupportedAttachMode {
        /// The mode the demand asked for, echoed back as the typed mode: a
        /// client that supports more than one branches on which of them was
        /// refused.
        mode: AttachMode,
    },
    /// The detail of `host/already-served`: the name already in service.
    #[serde(rename = "host/already-served")]
    #[non_exhaustive]
    AlreadyServed {
        /// The name an entry already stands under.
        name: VaultName,
    },
    /// The detail of `host/entry-held`: the name whose entry is held.
    #[serde(rename = "host/entry-held")]
    #[non_exhaustive]
    EntryHeld {
        /// The name the held entry stands under.
        name: VaultName,
    },
    /// The detail of `host/entry-not-ready`: where the entry stands.
    #[serde(rename = "host/entry-not-ready")]
    #[non_exhaustive]
    EntryNotReady {
        /// What the entry is doing instead of serving, with the counters a
        /// poll would have read.
        state: NotReady,
    },
    /// The detail of `host/reader-unavailable`: why the read seam is down.
    #[serde(rename = "host/reader-unavailable")]
    #[non_exhaustive]
    ReaderUnavailable {
        /// The refusal in words, for a person reading a message or a log.
        /// Clients never match on it.
        detail: String,
    },
    /// The detail of `host/registry-unwritable`: why the registry file could
    /// not be written.
    #[serde(rename = "host/registry-unwritable")]
    #[non_exhaustive]
    RegistryUnwritable {
        /// The write's own account of what refused, in words, for a person
        /// reading a message or a log. Clients never match on it.
        detail: String,
    },
    /// The detail of `vault/ambiguous-root`: every registered name whose
    /// registration contains the directory that was asked about. No entry is
    /// involved; the ask is a resolution of a directory.
    #[serde(rename = "vault/ambiguous-root")]
    #[non_exhaustive]
    AmbiguousRoot {
        /// The candidate names, in ascending order, each echoed back as the
        /// typed name.
        candidates: Vec<VaultName>,
    },
    /// The detail of `vault/ambiguous-target`: the target, the head of the
    /// documents it resolves to, and the request that enumerates the rest. It
    /// is the same bounded head and the same hint a finding over the class
    /// carries, so a refusal and a finding say one thing.
    #[serde(rename = "vault/ambiguous-target")]
    #[non_exhaustive]
    AmbiguousTarget {
        /// The target the request named.
        target: ResolutionTarget,
        /// The documents it resolves to, in the resolution ladder's order,
        /// and how many there were.
        head: CandidateHead,
        /// What to ask to see the whole class.
        hint: Hint,
    },
    /// The detail of `vault/unknown-target`: the target the request named.
    #[serde(rename = "vault/unknown-target")]
    #[non_exhaustive]
    UnknownTarget {
        /// The target the request named, echoed back as the typed target: the
        /// request named it through the grammar, and the refusal hands the
        /// same parsed value back.
        target: ResolutionTarget,
    },
    /// The detail of `vault/reload-busy`, which carries nothing.
    #[serde(rename = "vault/reload-busy")]
    ReloadBusy {},
    /// The detail of `vault/reload-failed`: what the reload met.
    #[serde(rename = "vault/reload-failed")]
    #[non_exhaustive]
    ReloadFailed {
        /// Why the reload did not leave the vault serving what its control
        /// files state.
        failure: ReloadFailure,
    },
    /// The detail of `vault/cursor-order-changed`: the order the cursor was
    /// minted under, and the order that stands.
    #[serde(rename = "vault/cursor-order-changed")]
    #[non_exhaustive]
    CursorOrderChanged {
        /// The order the cursor named and the order that stands, as the
        /// continuation's own account of the change.
        order: CursorOrderChanged,
    },
    /// The detail of `engine/not-enabled`: which rung, and what enables it.
    #[serde(rename = "engine/not-enabled")]
    #[non_exhaustive]
    EngineNotEnabled {
        /// The rung the request asked for.
        rung: Rung,
        /// What to do about it, in words, for a person reading a message.
        detail: String,
    },
    /// The detail of `engine/unavailable`: which rung, and why no engine
    /// stands for it.
    #[serde(rename = "engine/unavailable")]
    #[non_exhaustive]
    EngineUnavailable {
        /// The rung the request asked for.
        rung: Rung,
        /// Why no engine stands for it, in words, for a person reading a
        /// message or a log.
        detail: String,
    },
    /// The detail of `engine/failed`: which rung, and how the answer failed.
    #[serde(rename = "engine/failed")]
    #[non_exhaustive]
    EngineFailed {
        /// The rung the request asked for.
        rung: Rung,
        /// The failure in words, for a person reading a message or a log.
        detail: String,
    },
}

impl ErrorDetail {
    /// The detail of `host/duplicate-root`, for the colliding `aliases`.
    ///
    /// The aliases are sorted here, so the ascending order the field promises
    /// holds for every producer rather than for the ones that sorted first.
    pub fn duplicate_root(aliases: impl IntoIterator<Item = VaultName>) -> Self {
        let mut aliases: Vec<VaultName> = aliases.into_iter().collect();
        aliases.sort();
        ErrorDetail::DuplicateRoot { aliases }
    }

    /// The detail of `host/entry-untrusted`, for `reason`.
    pub const fn entry_untrusted(reason: UntrustedReason) -> Self {
        ErrorDetail::EntryUntrusted { reason }
    }

    /// The detail of `host/maintainer-contended`, for `incumbent`.
    pub const fn maintainer_contended(incumbent: MaintainerIdentity) -> Self {
        Self::MaintainerContended { incumbent }
    }

    /// The detail of `host/unknown-vault`, for the requested `name`.
    pub const fn unknown_vault(name: VaultName) -> Self {
        ErrorDetail::UnknownVault { name }
    }

    /// The detail of `host/unsupported-attach-mode`, for the `mode` the demand
    /// named.
    pub const fn unsupported_attach_mode(mode: AttachMode) -> Self {
        ErrorDetail::UnsupportedAttachMode { mode }
    }

    /// The detail of `host/already-served`, for the `name` already in
    /// service.
    pub const fn already_served(name: VaultName) -> Self {
        ErrorDetail::AlreadyServed { name }
    }

    /// The detail of `host/entry-held`, for the held entry's `name`.
    pub const fn entry_held(name: VaultName) -> Self {
        ErrorDetail::EntryHeld { name }
    }

    /// The detail of `host/entry-not-ready`, for where the entry stands.
    pub const fn entry_not_ready(state: NotReady) -> Self {
        ErrorDetail::EntryNotReady { state }
    }

    /// The detail of `host/reader-unavailable`, described by `detail`.
    pub fn reader_unavailable(detail: impl Into<String>) -> Self {
        ErrorDetail::ReaderUnavailable {
            detail: detail.into(),
        }
    }

    /// The detail of `host/registry-unwritable`, described by `detail`.
    pub fn registry_unwritable(detail: impl Into<String>) -> Self {
        ErrorDetail::RegistryUnwritable {
            detail: detail.into(),
        }
    }

    /// The detail of `vault/ambiguous-root`, for the `candidates` the root
    /// resolves under.
    ///
    /// The candidates are sorted here, so the ascending order the field
    /// promises holds for every producer rather than for the ones that sorted
    /// first.
    pub fn ambiguous_root(candidates: impl IntoIterator<Item = VaultName>) -> Self {
        let mut candidates: Vec<VaultName> = candidates.into_iter().collect();
        candidates.sort();
        ErrorDetail::AmbiguousRoot { candidates }
    }

    /// The detail of `vault/ambiguous-target`, for the `target` that resolves
    /// to the documents `head` heads.
    ///
    /// The head is the type a finding row carries, so the refusal and the
    /// finding carry one head bounded one way.
    pub const fn ambiguous_target(
        target: ResolutionTarget,
        head: CandidateHead,
        hint: Hint,
    ) -> Self {
        ErrorDetail::AmbiguousTarget { target, head, hint }
    }

    /// The detail of `vault/unknown-target`, for the `target` that resolves to
    /// no document.
    pub const fn unknown_target(target: ResolutionTarget) -> Self {
        ErrorDetail::UnknownTarget { target }
    }

    /// The detail of `vault/reload-busy`.
    pub const fn reload_busy() -> Self {
        ErrorDetail::ReloadBusy {}
    }

    /// The detail of `vault/reload-failed`, for what the reload met.
    pub const fn reload_failed(failure: ReloadFailure) -> Self {
        ErrorDetail::ReloadFailed { failure }
    }

    /// The detail of `vault/cursor-order-changed`, for the `order` that
    /// changed.
    pub const fn cursor_order_changed(order: CursorOrderChanged) -> Self {
        ErrorDetail::CursorOrderChanged { order }
    }

    /// The detail of `engine/not-enabled`, for `rung`, described by `detail`.
    pub fn engine_not_enabled(rung: Rung, detail: impl Into<String>) -> Self {
        ErrorDetail::EngineNotEnabled {
            rung,
            detail: detail.into(),
        }
    }

    /// The detail of `engine/unavailable`, for `rung`, described by `detail`.
    pub fn engine_unavailable(rung: Rung, detail: impl Into<String>) -> Self {
        ErrorDetail::EngineUnavailable {
            rung,
            detail: detail.into(),
        }
    }

    /// The detail of `engine/failed`, for `rung`, described by `detail`.
    pub fn engine_failed(rung: Rung, detail: impl Into<String>) -> Self {
        ErrorDetail::EngineFailed {
            rung,
            detail: detail.into(),
        }
    }

    /// The code this detail is the payload of.
    pub const fn code(&self) -> ReasonCode {
        match self {
            ErrorDetail::DuplicateRoot { .. } => ReasonCode::HostDuplicateRoot,
            ErrorDetail::EntryUntrusted { .. } => ReasonCode::HostEntryUntrusted,
            ErrorDetail::MaintainerContended { .. } => ReasonCode::HostMaintainerContended,
            ErrorDetail::UnknownVault { .. } => ReasonCode::HostUnknownVault,
            ErrorDetail::UnsupportedAttachMode { .. } => ReasonCode::HostUnsupportedAttachMode,
            ErrorDetail::AlreadyServed { .. } => ReasonCode::HostAlreadyServed,
            ErrorDetail::EntryHeld { .. } => ReasonCode::HostEntryHeld,
            ErrorDetail::EntryNotReady { .. } => ReasonCode::HostEntryNotReady,
            ErrorDetail::ReaderUnavailable { .. } => ReasonCode::HostReaderUnavailable,
            ErrorDetail::RegistryUnwritable { .. } => ReasonCode::HostRegistryUnwritable,
            ErrorDetail::AmbiguousRoot { .. } => ReasonCode::VaultAmbiguousRoot,
            ErrorDetail::AmbiguousTarget { .. } => ReasonCode::VaultAmbiguousTarget,
            ErrorDetail::UnknownTarget { .. } => ReasonCode::VaultUnknownTarget,
            ErrorDetail::ReloadBusy { .. } => ReasonCode::VaultReloadBusy,
            ErrorDetail::ReloadFailed { .. } => ReasonCode::VaultReloadFailed,
            ErrorDetail::CursorOrderChanged { .. } => ReasonCode::VaultCursorOrderChanged,
            ErrorDetail::EngineNotEnabled { .. } => ReasonCode::EngineNotEnabled,
            ErrorDetail::EngineUnavailable { .. } => ReasonCode::EngineUnavailable,
            ErrorDetail::EngineFailed { .. } => ReasonCode::EngineFailed,
        }
    }
}

/// A refusal, in the one shape every refusal takes.
///
/// Three fields and no others: the `code` a program switches on, the `message`
/// a person reads, and the `detail` the code pairs with. The code and the
/// detail name the same refusal: an envelope whose `code` is not the code its
/// `detail` carries does not parse.
///
/// **The detail is held behind one indirection**, which is invisible on the
/// wire and is what keeps the envelope narrow. Every fallible call in the
/// workspace returns `Result<_, ErrorEnvelope>`, so the widest detail any code
/// carries would otherwise be the width of every `Result` there is; one box
/// here prices a refusal's payload where the refusal is rather than at every
/// call that could produce one.
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ErrorEnvelope {
    code: ReasonCode,
    message: String,
    detail: Box<ErrorDetail>,
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
            detail: Box::new(detail),
        }
    }

    /// What was refused.
    pub const fn code(&self) -> &ReasonCode {
        &self.code
    }

    /// The refusal in words, for a person.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The code's typed payload.
    pub const fn detail(&self) -> &ErrorDetail {
        &self.detail
    }
}

/// The envelope as it arrives, before the code and the detail are checked
/// against each other. The field names and order are the envelope's, so the
/// bytes a reader accepts are the bytes a writer produces.
#[derive(Deserialize)]
struct EnvelopeFields {
    code: ReasonCode,
    message: String,
    detail: Box<ErrorDetail>,
}

impl<'de> Deserialize<'de> for ErrorEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = EnvelopeFields::deserialize(deserializer)?;
        let carried = fields.detail.code();
        if fields.code != carried {
            return Err(D::Error::custom(format!(
                "the envelope's code {:?} is not the code its detail carries, {carried:?}",
                fields.code
            )));
        }
        Ok(ErrorEnvelope {
            code: fields.code,
            message: fields.message,
            detail: fields.detail,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::document::DocumentPath;
    use crate::finding_row::Candidate;

    /// The target the two target refusals are read against, parsed through the
    /// grammar the type keeps.
    fn a_target() -> ResolutionTarget {
        ResolutionTarget::new("glossary").expect("a legal resolution target")
    }

    /// Every code the vocabulary holds, read back out of the schema the derive
    /// produces: the list is the enum's own, not one maintained beside it.
    fn every_code() -> Vec<(String, ReasonCode)> {
        let schema = serde_json::to_value(schemars::schema_for!(ReasonCode))
            .expect("a reason-code schema as JSON");
        let branches = schema["oneOf"]
            .as_array()
            .unwrap_or_else(|| panic!("the code schema describes no enum: {schema}"))
            .clone();
        assert!(!branches.is_empty(), "the code list is empty");
        branches
            .iter()
            .map(|branch| {
                let string = branch["const"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a code branch is not a pinned string: {branch}"))
                    .to_owned();
                let code = serde_json::from_str(&format!("\"{string}\""))
                    .unwrap_or_else(|error| panic!("reading the code {string} back: {error}"));
                (string, code)
            })
            .collect()
    }

    /// The string a code is on the wire. The match carries no wildcard, so a
    /// code minted without a row here does not compile.
    fn wire_string(code: &ReasonCode) -> &'static str {
        match code {
            ReasonCode::HostDuplicateRoot => "host/duplicate-root",
            ReasonCode::HostEntryUntrusted => "host/entry-untrusted",
            ReasonCode::HostMaintainerContended => "host/maintainer-contended",
            ReasonCode::HostUnknownVault => "host/unknown-vault",
            ReasonCode::HostUnsupportedAttachMode => "host/unsupported-attach-mode",
            ReasonCode::HostAlreadyServed => "host/already-served",
            ReasonCode::HostEntryHeld => "host/entry-held",
            ReasonCode::HostEntryNotReady => "host/entry-not-ready",
            ReasonCode::HostReaderUnavailable => "host/reader-unavailable",
            ReasonCode::HostRegistryUnwritable => "host/registry-unwritable",
            ReasonCode::VaultAmbiguousRoot => "vault/ambiguous-root",
            ReasonCode::VaultAmbiguousTarget => "vault/ambiguous-target",
            ReasonCode::VaultUnknownTarget => "vault/unknown-target",
            ReasonCode::VaultReloadBusy => "vault/reload-busy",
            ReasonCode::VaultReloadFailed => "vault/reload-failed",
            ReasonCode::VaultCursorOrderChanged => "vault/cursor-order-changed",
            ReasonCode::EngineNotEnabled => "engine/not-enabled",
            ReasonCode::EngineUnavailable => "engine/unavailable",
            ReasonCode::EngineFailed => "engine/failed",
        }
    }

    /// A detail the code pairs with, carrying one payload that code can hold.
    /// The match carries no wildcard, so a code minted without a detail does
    /// not compile.
    fn a_detail(code: &ReasonCode) -> ErrorDetail {
        match code {
            ReasonCode::HostDuplicateRoot => ErrorDetail::duplicate_root(
                ["notes", "vault"].map(|text| VaultName::new(text).expect("a legal vault name")),
            ),
            ReasonCode::HostEntryUntrusted => {
                ErrorDetail::entry_untrusted(UntrustedReason::WatcherOverflow)
            }
            ReasonCode::HostMaintainerContended => ErrorDetail::maintainer_contended(
                MaintainerIdentity::named(41, "0.1.0", 1_700_000_000),
            ),
            ReasonCode::HostUnknownVault => {
                ErrorDetail::unknown_vault(VaultName::new("notes").expect("a legal vault name"))
            }
            ReasonCode::HostUnsupportedAttachMode => {
                ErrorDetail::unsupported_attach_mode(AttachMode::Throwaway)
            }
            ReasonCode::HostAlreadyServed => {
                ErrorDetail::already_served(VaultName::new("notes").expect("a legal vault name"))
            }
            ReasonCode::HostEntryHeld => {
                ErrorDetail::entry_held(VaultName::new("notes").expect("a legal vault name"))
            }
            ReasonCode::HostEntryNotReady => ErrorDetail::entry_not_ready(NotReady::unattached()),
            ReasonCode::HostReaderUnavailable => {
                ErrorDetail::reader_unavailable("this coverage mints no read handle")
            }
            ReasonCode::HostRegistryUnwritable => {
                ErrorDetail::registry_unwritable("the registry file is read-only")
            }
            ReasonCode::VaultAmbiguousRoot => ErrorDetail::ambiguous_root(
                ["notes", "vault"].map(|text| VaultName::new(text).expect("a legal vault name")),
            ),
            ReasonCode::VaultAmbiguousTarget => ErrorDetail::ambiguous_target(
                a_target(),
                CandidateHead::new(
                    [Candidate::new(
                        DocumentPath::new("notes/glossary.md").expect("a legal document path"),
                        "notes/glossary",
                    )],
                    2,
                )
                .expect("a head no larger than its total"),
                Hint::resolves(a_target()),
            ),
            ReasonCode::VaultUnknownTarget => ErrorDetail::unknown_target(a_target()),
            ReasonCode::VaultReloadBusy => ErrorDetail::reload_busy(),
            ReasonCode::VaultReloadFailed => {
                ErrorDetail::reload_failed(ReloadFailure::unsupported())
            }
            ReasonCode::VaultCursorOrderChanged => ErrorDetail::cursor_order_changed(
                CursorOrderChanged::new("fp-1", Some("fp-2".to_string())),
            ),
            ReasonCode::EngineNotEnabled => ErrorDetail::engine_not_enabled(
                Rung::Vector,
                "enable the engine section in .norn/config.toml and run vault reload",
            ),
            ReasonCode::EngineUnavailable => {
                ErrorDetail::engine_unavailable(Rung::Vector, "the engine slot is empty")
            }
            ReasonCode::EngineFailed => {
                ErrorDetail::engine_failed(Rung::Vector, "the answer failed")
            }
        }
    }

    #[test]
    fn every_code_serializes_to_the_string_it_is_written_as() {
        for (string, code) in every_code() {
            assert_eq!(wire_string(&code), string);
            assert_eq!(
                serde_json::to_string(&code).expect("serializing a code"),
                format!("\"{string}\"")
            );
        }
    }

    /// The shape of a code, not just its spelling: one namespace, one slash,
    /// and lowercase kebab-case on both sides of it.
    #[test]
    fn every_code_is_one_namespace_and_one_kebab_case_name() {
        for (string, _) in every_code() {
            let (namespace, name) = string
                .split_once('/')
                .unwrap_or_else(|| panic!("`{string}` carries no namespace"));
            assert!(
                !name.contains('/'),
                "`{string}` carries more than one separator"
            );
            for segment in [namespace, name] {
                assert!(!segment.is_empty(), "`{string}` has an empty segment");
                assert!(
                    segment
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                    "`{string}` is not lowercase kebab-case"
                );
                assert!(
                    !segment.starts_with('-') && !segment.ends_with('-'),
                    "`{string}` has a segment bounded by a hyphen"
                );
            }
        }
    }

    /// Each code has a detail, and that detail names the code back.
    #[test]
    fn every_code_pairs_with_a_detail_that_names_it_back() {
        for (string, code) in every_code() {
            let detail = a_detail(&code);
            assert_eq!(detail.code(), code, "the detail of {string} names another");
            let json = serde_json::to_value(&detail).expect("a detail as JSON");
            assert_eq!(json["code"].as_str(), Some(string.as_str()));
        }
    }

    /// The pairing is the read path's, not the constructor's: every code is
    /// tried against every detail, and only the pairs that name one refusal
    /// parse.
    #[test]
    fn an_envelope_parses_only_when_its_code_is_the_code_its_detail_carries() {
        for (string, _) in every_code() {
            for (_, code) in every_code() {
                let detail = serde_json::to_string(&a_detail(&code)).expect("a detail as JSON");
                let json =
                    format!(r#"{{"code":"{string}","message":"refused","detail":{detail}}}"#);
                let read = serde_json::from_str::<ErrorEnvelope>(&json);
                assert_eq!(
                    read.is_ok(),
                    string == wire_string(&code),
                    "reading {json} answered {read:?}"
                );
            }
        }
    }
}
