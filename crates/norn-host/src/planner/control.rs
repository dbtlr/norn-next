//! Control targets: where a control-file write's role lives in the vault, and
//! what its content must read as.
//!
//! **A control file is no document.** The vault schema and the vault config
//! are what every document is judged under, so a `write_control_file` names
//! its file by role and the planner maps the role here to the one in-vault
//! path the host reads it at, `norn-config`'s convention: the schema at
//! [`IN_VAULT_SCHEMA_PATH`] and the config at [`IN_VAULT_CONFIG_PATH`]. The
//! planner and the applier read a control target through
//! [`VaultView::control_entry`](super::view::VaultView::control_entry) rather
//! than as a document, since the vault's walk does not enter the schema's path;
//! the applier judges its content by [`unreadable_as_role`] rather than under
//! the schema; no link reads one ([`super::links::Target`]); and the changeset
//! records no document row for one.
//!
//! **What a control target's content must be.** A write lands only content its
//! role's parser reads — `norn-config`'s [`VaultSchema::parse`] for the schema
//! and [`VaultConfig::parse`] for the config — so a plan never publishes a
//! control file the next reload would refuse.

use std::sync::LazyLock;

use norn_config::schema::VaultSchema;
use norn_config::vault::VaultConfig;
use norn_config::{IN_VAULT_CONFIG_PATH, IN_VAULT_SCHEMA_PATH};
use norn_wire::{ControlFile, DocumentPath};

/// Where the vault schema a `write_control_file` writes lives.
static SCHEMA_PATH: LazyLock<DocumentPath> = LazyLock::new(|| {
    DocumentPath::new(IN_VAULT_SCHEMA_PATH).expect("the schema convention is a vault path")
});

/// Where the vault config a `write_control_file` writes lives.
static CONFIG_PATH: LazyLock<DocumentPath> = LazyLock::new(|| {
    DocumentPath::new(IN_VAULT_CONFIG_PATH).expect("the config convention is a vault path")
});

/// The in-vault path the control file `file` lives at.
pub(crate) fn control_path(file: ControlFile) -> &'static DocumentPath {
    match file {
        ControlFile::Schema => &SCHEMA_PATH,
        ControlFile::Config => &CONFIG_PATH,
    }
}

/// The role of the control file at the vault path `path`, where `path` is
/// where one lives.
pub(crate) fn role_at(path: &str) -> Option<ControlFile> {
    [ControlFile::Schema, ControlFile::Config]
        .into_iter()
        .find(|&file| control_path(file).as_str() == path)
}

/// Why `content` cannot be the control file `file`, in words, or `None` where
/// its role's parser reads it.
pub(crate) fn unreadable_as_role(file: ControlFile, content: &[u8]) -> Option<String> {
    match file {
        ControlFile::Schema => VaultSchema::parse(content)
            .err()
            .map(|error| format!("the content does not read as a vault schema: {error}")),
        ControlFile::Config => VaultConfig::parse(Some(content))
            .err()
            .map(|error| format!("the content does not read as a vault config: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Each role maps to the path the host reads it at, and back.**
    #[test]
    fn each_role_lives_at_the_path_the_host_reads_it_at() {
        assert_eq!(
            control_path(ControlFile::Schema).as_str(),
            ".norn/schema.yaml"
        );
        assert_eq!(
            control_path(ControlFile::Config).as_str(),
            ".norn/config.toml"
        );
        for file in [ControlFile::Schema, ControlFile::Config] {
            assert_eq!(role_at(control_path(file).as_str()), Some(file));
        }
        assert_eq!(role_at(".norn/notes.md"), None);
    }
}
