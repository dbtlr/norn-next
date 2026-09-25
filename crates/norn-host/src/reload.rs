//! Typed input and diagnostics for one vault reload candidate, and the
//! `vault reload` handler that answers through them.

use std::fmt;
use std::path::{Path, PathBuf};

use norn_config::schema::VaultSchema;
use norn_config::vault::{VaultConfig, VaultConfigError};
use norn_config::{IN_VAULT_CONFIG_PATH, IN_VAULT_SCHEMA_PATH};
use norn_fs::{ContentHash, Refusal};
use norn_wire::{ErrorEnvelope, ReloadParams, ReloadReport, TrustState, VaultName};

use crate::address::registered_name;
use crate::lifecycle::{Demand, EntryOps, Host, HostError};
use crate::{JobFailure, Registration};

/// A registered engine boundary that receives one vault's optional parsed
/// section. The receiver owns all meaning and all effects after this call.
pub trait EngineConfigReceiver: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn receive(&self, vault: &VaultName, config: Option<&norn_config::vault::EngineConfig>);
}

/// Which authored control file a reload diagnostic names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReloadFile {
    Schema,
    Config,
}

/// Which core boundary refused a reload candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReloadStage {
    Read,
    Parse,
    Apply,
}

/// A core reload error retained for internal inspection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReloadError {
    SchemaRead(Refusal),
    SchemaParse(String),
    ConfigRead(Refusal),
    ConfigParse(VaultConfigError),
    SchemaApply(String),
}

impl ReloadError {
    pub fn file(&self) -> ReloadFile {
        match self {
            Self::SchemaRead(_) | Self::SchemaParse(_) | Self::SchemaApply(_) => ReloadFile::Schema,
            Self::ConfigRead(_) | Self::ConfigParse(_) => ReloadFile::Config,
        }
    }

    pub fn stage(&self) -> ReloadStage {
        match self {
            Self::SchemaRead(_) | Self::ConfigRead(_) => ReloadStage::Read,
            Self::SchemaParse(_) | Self::ConfigParse(_) => ReloadStage::Parse,
            Self::SchemaApply(_) => ReloadStage::Apply,
        }
    }
}

impl fmt::Display for ReloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SchemaRead(error) => write!(f, "the vault schema cannot be read: {error}"),
            Self::SchemaParse(error) => write!(f, "the vault schema is invalid: {error}"),
            Self::ConfigRead(error) => write!(f, "the vault config cannot be read: {error}"),
            Self::ConfigParse(error) => write!(f, "the vault config is invalid: {error}"),
            Self::SchemaApply(error) => write!(f, "the vault schema cannot be applied: {error}"),
        }
    }
}

impl std::error::Error for ReloadError {}

/// Whether the active config came from a file or from its missing-file default.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigFingerprint {
    Missing,
    File(ContentHash),
}

/// The active fingerprints retained for internal inspection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActiveFingerprints {
    pub schema: ContentHash,
    pub config: ConfigFingerprint,
}

/// What a reload decided about a validated candidate's schema. The config is
/// taken into service either way.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReloadOutcome {
    ConfigOnly,
    SchemaChanged,
}

impl From<ActiveFingerprints> for norn_wire::Fingerprints {
    /// Each fingerprint as 64 lowercase hex digits, and no config where the
    /// vault serves the missing-file default.
    fn from(active: ActiveFingerprints) -> Self {
        let fingerprints = norn_wire::Fingerprints::new(active.schema.to_hex());
        match active.config {
            ConfigFingerprint::Missing => fingerprints,
            ConfigFingerprint::File(config) => fingerprints.with_config(config.to_hex()),
        }
    }
}

impl From<ReloadOutcome> for norn_wire::ReloadOutcome {
    fn from(outcome: ReloadOutcome) -> Self {
        match outcome {
            ReloadOutcome::ConfigOnly => norn_wire::ReloadOutcome::ConfigOnly,
            ReloadOutcome::SchemaChanged => norn_wire::ReloadOutcome::SchemaChanged,
        }
    }
}

/// What a reload judged its candidate to be: the outcome it decided about the
/// schema, and the fingerprints the candidate was read at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReloadJudgment {
    pub outcome: ReloadOutcome,
    pub fingerprints: ActiveFingerprints,
}

impl<O: EntryOps> Host<O> {
    /// Answer a `vault reload`: validate the vault's schema-and-config
    /// candidate and activate it, or with `dry_run` validate it and activate
    /// nothing.
    ///
    /// Both are admitted and run as one reload is, so a dry run is refused
    /// whenever an activation asked at the same moment would be — among them
    /// `vault/reload-busy` while something works over the vault — and answers
    /// the outcome and fingerprints that activation would. A runtime failure
    /// either meets gets the same policy, and a reload the host drops without
    /// an answer — moved past before it ran, or lost with a leg that unwound —
    /// answers where the entry stands once it is dropped. Where the entry
    /// stands is its published demand, so an entry an unregistration holds is
    /// refused as held, a name the host no longer serves as unknown, and an
    /// entry standing on a park with the park's own code, as `vault status`
    /// answers each. Every refusal renders through [`ReloadRefusal::answer`].
    ///
    /// `Err(HostError)` is a host shutting down or whose job channel is gone,
    /// which is transport death and carries no code: nothing about the vault
    /// was learned.
    pub fn vault_reload(
        &self,
        params: &ReloadParams,
    ) -> Result<Result<ReloadReport, ErrorEnvelope>, HostError> {
        let name = match registered_name(&params.vault) {
            Ok(name) => name,
            Err(refused) => return Ok(Err(refused)),
        };
        let judged = if params.dry_run {
            self.judge_reload(name)
        } else {
            self.reload(name)
        };
        match judged {
            Ok(judgment) => {
                let report =
                    ReloadReport::new(judgment.outcome.into(), judgment.fingerprints.into());
                Ok(Ok(if params.dry_run {
                    report.validated()
                } else {
                    report
                }))
            }
            Err(refusal) => refusal.answer(name).map(Err),
        }
    }
}

/// Why one internal reload request did not return a Ready vault.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReloadRefusal {
    Unsupported,
    /// The entry is not reloadable right now, and this is where it stands:
    /// its published demand — the one reading `vault status` and a demand
    /// lease answer the same name with. A name the host does not serve and an
    /// entry an unregistration holds answer as every door answers them; an
    /// entry in service answers with the park it stands on first, and its
    /// trust state where nothing parks it.
    Unavailable(Demand),
    Core(ReloadError),
    Runtime(JobFailure),
    HostStopped,
}

/// The retained core reload facts for one served vault.
///
/// Read through [`crate::Host::inspect`], and reported by `vault status` out
/// of the same observation; retained so a status finds them without
/// re-reading a control file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultInspection {
    pub trust: TrustState,
    pub active_fingerprints: Option<ActiveFingerprints>,
    pub last_reload_error: Option<ReloadError>,
    /// Why this entry's reads refuse, where its coverage minted no read
    /// handle.
    ///
    /// A retained fact of the same kind as the reload diagnostic beside it: it
    /// does not move the trust state, because an entry whose read seam is down
    /// is still serving every other surface. It is read here and reported as
    /// itself, so a status answer says "served, reads refusing, and this is
    /// why" rather than leaving a client to infer it from a refused read.
    pub reader_unavailable: Option<crate::ReaderUnavailable>,
    /// What the entry's last attachment met in its environment that is worth
    /// telling an operator: the advisories [`crate::EntryOps::advisories`]
    /// read at its last publication, kept past the release of its coverage.
    pub advisories: Vec<crate::AttachmentAdvisory>,
}

/// The authored control-file state relative to the active fingerprints.
///
/// Answered through [`crate::Host::authored_drift`] and reported by `vault
/// status`: the reading that says a reload is pending, since no watcher event
/// ever will.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthoredDrift {
    Inactive,
    Current,
    ReloadPending,
    Unreadable(ReloadError),
}

impl AuthoredDrift {
    /// The authored control files of `registration` against `active`, read
    /// now from `control_root` — the root the active controls were read
    /// from — or from the registered root where the entry holds none.
    ///
    /// No active fingerprints is an entry serving no controls yet, which
    /// reads nothing. Otherwise both files are fingerprinted without being
    /// parsed, outside any lock.
    ///
    /// **Both are read whole, with no bound.** No size contract binds a
    /// control file: an attach, a recovery and a reload read the same two
    /// files whole to serve under them. So a status reads no more than the
    /// read that put the active fingerprints into service, and a bound here
    /// would report as unreadable a file the entry serves under. What a
    /// status pays for the drift is linear in the two files' size, which is
    /// the vault author's.
    pub(crate) fn of(
        registration: &Registration,
        active: Option<ActiveFingerprints>,
        control_root: Option<&Path>,
    ) -> Self {
        let Some(active) = active else {
            return AuthoredDrift::Inactive;
        };
        match ReloadCandidate::authored_fingerprints_at(
            registration,
            control_root.unwrap_or_else(|| registration.root.as_path()),
        ) {
            Ok(authored) if authored == active => AuthoredDrift::Current,
            Ok(_) => AuthoredDrift::ReloadPending,
            Err(error) => AuthoredDrift::Unreadable(error),
        }
    }
}

impl From<AuthoredDrift> for norn_wire::Drift {
    /// The drift as a status reports it, an unreadable file named by the
    /// control-file failure a refused reload names it by.
    fn from(drift: AuthoredDrift) -> Self {
        match drift {
            AuthoredDrift::Inactive => norn_wire::Drift::inactive(),
            AuthoredDrift::Current => norn_wire::Drift::current(),
            AuthoredDrift::ReloadPending => norn_wire::Drift::reload_pending(),
            AuthoredDrift::Unreadable(error) => {
                norn_wire::Drift::unreadable(crate::refusal::control_file_failure(&error))
            }
        }
    }
}

/// One fully read and core-validated reload candidate.
///
/// **Reading the schema's bytes and reading its declaration are two
/// questions.** Bytes that cannot be read at all refuse here, because there is
/// no candidate without them. A declaration this build cannot act on is carried
/// as [`ReloadCandidate::undeclarable`] instead, so the two callers can answer
/// it differently: a reload refuses and leaves the vault serving the
/// declaration it already has, while an attach has no such declaration to fall
/// back on and publishes the cause over live coverage rather than hiding the
/// vault behind a refusal.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReloadCandidate {
    schema_bytes: Vec<u8>,
    config: VaultConfig,
    fingerprints: ActiveFingerprints,
    undeclarable: Option<String>,
}

impl ReloadCandidate {
    pub(crate) fn authored_fingerprints_at(
        registration: &Registration,
        covered_root: &Path,
    ) -> Result<ActiveFingerprints, ReloadError> {
        let (schema_anchor, schema_name) = schema_anchor_at(registration, covered_root)?;
        let schema = norn_fs::read_and_hash(&schema_anchor, &schema_name)
            .map_err(ReloadError::SchemaRead)?;
        let config =
            norn_fs::read_if_present_and_hash(covered_root, Path::new(IN_VAULT_CONFIG_PATH))
                .map_err(ReloadError::ConfigRead)?;
        Ok(fingerprints(&schema, config.as_ref()))
    }

    #[cfg(test)]
    pub(crate) fn read(registration: &Registration) -> Result<Self, ReloadError> {
        Self::read_at(registration, registration.root.as_path())
    }

    /// Read controls from the operational root one attachment covers.
    pub(crate) fn read_at(
        registration: &Registration,
        covered_root: &Path,
    ) -> Result<Self, ReloadError> {
        let (schema_anchor, schema_name) = schema_anchor_at(registration, covered_root)?;
        let schema = norn_fs::read_and_hash(&schema_anchor, &schema_name)
            .map_err(ReloadError::SchemaRead)?;
        // The schema is read into its content model here and nowhere else on
        // this path, so every caller reads one declaration out of one set of
        // bytes. What it means for that reading to fail is the caller's to
        // decide, which is why it is carried rather than raised.
        let undeclarable = VaultSchema::parse(schema.bytes())
            .err()
            .map(|error| error.to_string());

        let config =
            norn_fs::read_if_present_and_hash(covered_root, Path::new(IN_VAULT_CONFIG_PATH))
                .map_err(ReloadError::ConfigRead)?;
        let parsed = VaultConfig::parse(config.as_ref().map(norn_fs::ReadAndHash::bytes))
            .map_err(ReloadError::ConfigParse)?;
        let fingerprints = fingerprints(&schema, config.as_ref());
        let (schema_bytes, _) = schema.into_parts();
        Ok(Self {
            schema_bytes,
            config: parsed,
            fingerprints,
            undeclarable,
        })
    }

    /// Why this build cannot act on the candidate's declaration, in words, and
    /// `None` where it can.
    pub(crate) fn undeclarable(&self) -> Option<&str> {
        self.undeclarable.as_deref()
    }

    pub(crate) fn schema_bytes(&self) -> &[u8] {
        &self.schema_bytes
    }

    pub(crate) fn config(&self) -> &VaultConfig {
        &self.config
    }

    pub(crate) fn fingerprints(&self) -> ActiveFingerprints {
        self.fingerprints
    }
}

fn schema_anchor_at(
    registration: &Registration,
    covered_root: &Path,
) -> Result<(PathBuf, PathBuf), ReloadError> {
    let Some(source) = registration.schema_source.as_ref() else {
        return Ok((covered_root.to_owned(), PathBuf::from(IN_VAULT_SCHEMA_PATH)));
    };
    if let Ok(relative) = source.as_path().strip_prefix(registration.root.as_path()) {
        let operational = covered_root.join(relative);
        return schema_anchor_from_source(&operational);
    }
    schema_anchor_from_source(source.as_path())
}

fn fingerprints(
    schema: &norn_fs::ReadAndHash,
    config: Option<&norn_fs::ReadAndHash>,
) -> ActiveFingerprints {
    ActiveFingerprints {
        schema: schema.content_hash(),
        config: config.map_or(ConfigFingerprint::Missing, |read| {
            ConfigFingerprint::File(read.content_hash())
        }),
    }
}

fn schema_anchor_from_source(source: &Path) -> Result<(PathBuf, PathBuf), ReloadError> {
    let (Some(directory), Some(name)) = (source.parent(), source.file_name()) else {
        return Err(ReloadError::SchemaParse(format!(
            "schema source names no file: {}",
            source.display()
        )));
    };
    Ok((directory.to_owned(), PathBuf::from(name)))
}
