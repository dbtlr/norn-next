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
//! The pairing between a code and its detail is structural rather than a rule
//! constructors keep: [`ErrorEnvelope::new`] takes the code from the detail,
//! and the read path refuses an envelope whose code is not the code its detail
//! carries. `ReasonCode` is matched without a wildcard in this module's tests,
//! so a code minted without its detail does not compile.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::trust::UntrustedReason;

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
        /// The colliding names, in ascending order.
        aliases: Vec<String>,
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
        /// The vault name the request asked for, echoed back as typed data.
        name: String,
    },
}

impl ErrorDetail {
    /// The detail of `host/duplicate-root`, for the colliding `aliases`.
    ///
    /// The aliases are sorted here, so the ascending order the field promises
    /// holds for every producer rather than for the ones that sorted first.
    pub fn duplicate_root<A: Into<String>>(aliases: impl IntoIterator<Item = A>) -> Self {
        let mut aliases: Vec<String> = aliases.into_iter().map(Into::into).collect();
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
    pub fn unknown_vault(name: impl Into<String>) -> Self {
        ErrorDetail::UnknownVault { name: name.into() }
    }

    /// The code this detail is the payload of.
    pub const fn code(&self) -> ReasonCode {
        match self {
            ErrorDetail::DuplicateRoot { .. } => ReasonCode::HostDuplicateRoot,
            ErrorDetail::EntryUntrusted { .. } => ReasonCode::HostEntryUntrusted,
            ErrorDetail::MaintainerContended { .. } => ReasonCode::HostMaintainerContended,
            ErrorDetail::UnknownVault { .. } => ReasonCode::HostUnknownVault,
        }
    }
}

/// A refusal, in the one shape every refusal takes.
///
/// Three fields and no others: the `code` a program switches on, the `message`
/// a person reads, and the `detail` the code pairs with. The code and the
/// detail name the same refusal: an envelope whose `code` is not the code its
/// `detail` carries does not parse.
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ErrorEnvelope {
    code: ReasonCode,
    message: String,
    detail: ErrorDetail,
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
    detail: ErrorDetail,
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
        }
    }

    /// A detail the code pairs with, carrying one payload that code can hold.
    /// The match carries no wildcard, so a code minted without a detail does
    /// not compile.
    fn a_detail(code: &ReasonCode) -> ErrorDetail {
        match code {
            ReasonCode::HostDuplicateRoot => ErrorDetail::duplicate_root(["notes", "vault"]),
            ReasonCode::HostEntryUntrusted => {
                ErrorDetail::entry_untrusted(UntrustedReason::WatcherOverflow)
            }
            ReasonCode::HostMaintainerContended => ErrorDetail::maintainer_contended(
                MaintainerIdentity::named(41, "0.1.0", 1_700_000_000),
            ),
            ReasonCode::HostUnknownVault => ErrorDetail::unknown_vault("notes"),
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
