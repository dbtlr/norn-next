//! The generic per-vault config envelope.

use std::collections::BTreeMap;
use std::fmt;

use toml::{Table, Value};

/// Parsed config sections keyed by engine name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VaultConfig {
    engines: BTreeMap<String, EngineConfig>,
}

impl VaultConfig {
    /// Parses an optional config file image.
    pub fn parse(bytes: Option<&[u8]>) -> Result<Self, VaultConfigError> {
        let Some(bytes) = bytes else {
            return Ok(Self::default());
        };
        let text = std::str::from_utf8(bytes).map_err(|error| VaultConfigError::InvalidUtf8 {
            message: error.to_string(),
        })?;
        let mut document: Table =
            text.parse()
                .map_err(|error: toml::de::Error| VaultConfigError::InvalidToml {
                    message: error.to_string(),
                })?;
        let Some(engine) = document.remove("engine") else {
            return Ok(Self::default());
        };
        let Value::Table(engine) = engine else {
            return Err(VaultConfigError::InvalidEngineTable {
                found: engine.type_str(),
            });
        };
        let engines = engine
            .into_iter()
            .map(|(name, value)| match value {
                Value::Table(section) => Ok((name, EngineConfig(section))),
                other => Err(VaultConfigError::InvalidEngineSection {
                    name,
                    found: other.type_str(),
                }),
            })
            .collect::<Result<_, _>>()?;
        Ok(VaultConfig { engines })
    }

    /// The section for `name`, if the file supplies one.
    pub fn engine(&self, name: &str) -> Option<&EngineConfig> {
        self.engines.get(name)
    }
}

/// One engine-owned config section.
#[derive(Clone, Debug, PartialEq)]
pub struct EngineConfig(Table);

impl EngineConfig {
    /// The parsed engine-owned table.
    pub fn table(&self) -> &Table {
        &self.0
    }
}

/// Why config bytes are not a valid generic envelope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VaultConfigError {
    /// The file is not text.
    InvalidUtf8 { message: String },
    /// The text is not TOML.
    InvalidToml { message: String },
    /// The known top-level `engine` key does not hold a table.
    InvalidEngineTable { found: &'static str },
    /// One named engine does not hold a table.
    InvalidEngineSection { name: String, found: &'static str },
}

impl fmt::Display for VaultConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VaultConfigError::InvalidUtf8 { message } => {
                write!(f, "the vault config is not UTF-8: {message}")
            }
            VaultConfigError::InvalidToml { message } => {
                write!(f, "the vault config is not TOML: {message}")
            }
            VaultConfigError::InvalidEngineTable { found } => {
                write!(f, "`engine` is {found}, and it must be a table")
            }
            VaultConfigError::InvalidEngineSection { name, found } => {
                write!(f, "`engine.{name}` is {found}, and it must be a table")
            }
        }
    }
}

impl std::error::Error for VaultConfigError {}
