//! Typed input and diagnostics for one vault reload candidate.

use std::fmt;
use std::path::{Path, PathBuf};

use norn_config::vault::{VaultConfig, VaultConfigError};
use norn_config::{IN_VAULT_CONFIG_PATH, IN_VAULT_SCHEMA_PATH};
use norn_fs::{ContentHash, Refusal};
use norn_wire::{TrustState, VaultName};

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

/// Which core-controlled part of a validated candidate changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReloadOutcome {
    ConfigOnly,
    SchemaChanged,
}

/// Why one internal reload request did not return a Ready vault.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReloadRefusal {
    UnknownVault,
    Unsupported,
    Unavailable(TrustState),
    Core(ReloadError),
    Runtime(JobFailure),
    HostStopped,
}

/// The retained core reload facts for one served vault.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultInspection {
    pub trust: TrustState,
    pub active_fingerprints: Option<ActiveFingerprints>,
    pub last_reload_error: Option<ReloadError>,
}

/// The authored control-file state relative to the active fingerprints.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthoredDrift {
    Inactive,
    Current,
    ReloadPending,
    Unreadable(ReloadError),
}

/// One fully read and core-validated reload candidate.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReloadCandidate {
    schema_bytes: Vec<u8>,
    config: VaultConfig,
    fingerprints: ActiveFingerprints,
}

impl ReloadCandidate {
    pub(crate) fn authored_fingerprints(
        registration: &Registration,
    ) -> Result<ActiveFingerprints, ReloadError> {
        let (schema_anchor, schema_name) = schema_anchor(registration)?;
        let schema = norn_fs::read_and_hash(&schema_anchor, &schema_name)
            .map_err(ReloadError::SchemaRead)?;
        let config = norn_fs::read_if_present_and_hash(
            registration.root.as_path(),
            Path::new(IN_VAULT_CONFIG_PATH),
        )
        .map_err(ReloadError::ConfigRead)?;
        Ok(fingerprints(&schema, config.as_ref()))
    }

    pub(crate) fn read(registration: &Registration) -> Result<Self, ReloadError> {
        let (schema_anchor, schema_name) = schema_anchor(registration)?;
        let schema = norn_fs::read_and_hash(&schema_anchor, &schema_name)
            .map_err(ReloadError::SchemaRead)?;
        let schema_text = std::str::from_utf8(schema.bytes())
            .map_err(|error| ReloadError::SchemaParse(error.to_string()))?;
        serde_yaml::from_str::<serde_yaml::Value>(schema_text)
            .map_err(|error| ReloadError::SchemaParse(error.to_string()))?;

        let config = norn_fs::read_if_present_and_hash(
            registration.root.as_path(),
            Path::new(IN_VAULT_CONFIG_PATH),
        )
        .map_err(ReloadError::ConfigRead)?;
        let parsed = VaultConfig::parse(config.as_ref().map(norn_fs::ReadAndHash::bytes))
            .map_err(ReloadError::ConfigParse)?;
        let fingerprints = fingerprints(&schema, config.as_ref());
        let (schema_bytes, _) = schema.into_parts();
        Ok(Self {
            schema_bytes,
            config: parsed,
            fingerprints,
        })
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

/// The directory a schema read is anchored at, and its name below that anchor.
fn schema_anchor(registration: &Registration) -> Result<(PathBuf, PathBuf), ReloadError> {
    let Some(source) = registration.schema_source.as_ref() else {
        return Ok((
            registration.root.as_path().to_owned(),
            PathBuf::from(IN_VAULT_SCHEMA_PATH),
        ));
    };
    let source = source.as_path();
    let (Some(directory), Some(name)) = (source.parent(), source.file_name()) else {
        return Err(ReloadError::SchemaParse(format!(
            "schema source names no file: {}",
            source.display()
        )));
    };
    Ok((directory.to_owned(), PathBuf::from(name)))
}
