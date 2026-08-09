//! Scratch machines, and the shapes the suite writes into them.
//!
//! Every case here works over a [`Scratch`]: two temporary base directories
//! standing in for the XDG bases, and the [`ConfigDirs`] built over them. No
//! case reads or sets an environment variable — that is the one entry point
//! the environment suite exercises, and it lives in its own binary so that
//! nothing here runs beside a process-wide write.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use norn_config::machine::tokens::TokenLabel;
use norn_config::registry::{Entry, VaultRoot};
use norn_config::{ConfigDirs, VaultName};

/// Distinguishes two scratch machines taken in the same process.
static SERIAL: AtomicU64 = AtomicU64::new(0);

/// A machine's config and data bases, for the length of one test.
pub struct Scratch {
    root: PathBuf,
    dirs: ConfigDirs,
}

impl Scratch {
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the bases a test's state lives under.
    pub fn new(label: &str) -> Self {
        let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "norn-config-{label}-{}-{serial}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config")).expect("a config base");
        std::fs::create_dir_all(root.join("data")).expect("a data base");
        let dirs = ConfigDirs::new(root.join("config"), root.join("data"))
            .expect("absolute scratch bases");
        Scratch { root, dirs }
    }

    pub fn dirs(&self) -> &ConfigDirs {
        &self.dirs
    }

    pub fn config_base(&self) -> PathBuf {
        self.root.join("config")
    }

    pub fn data_base(&self) -> PathBuf {
        self.root.join("data")
    }

    /// Where the registry file sits.
    ///
    /// Spelled here rather than asked of [`ConfigDirs`]: the crate publishes no
    /// path to either data file, because a path is a way to open one without
    /// the lock, the mode and the replacement protocol the API brings. A test
    /// that judges the bytes rebuilds the convention from the directory the
    /// crate does publish.
    pub fn registry_file(&self) -> PathBuf {
        self.dirs.config_dir().join("registry.toml")
    }

    /// Where the token file sits, on the same terms.
    pub fn tokens_file(&self) -> PathBuf {
        self.dirs.config_dir().join("tokens.toml")
    }

    /// The config directory, made the way the crate makes it.
    ///
    /// A case that arranges something inside it needs it to be there first,
    /// and a case about the token file needs it owner-only — the token API
    /// refuses a directory the group or the world can reach into, whatever the
    /// file's own mode says.
    #[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Harness scaffolding: the directory the crate creates.
    pub fn make_config_dir(&self) -> PathBuf {
        use std::os::unix::fs::DirBuilderExt;
        let directory = self.dirs.config_dir().to_path_buf();
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)
            .expect("a config directory");
        directory
    }

    /// Put `text` at `path`, making its directory first. Used to arrange a
    /// file the API would never write — a version ahead of this build, a
    /// document with fields it does not model, bytes that are not TOML.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging bytes the API refuses to write.
    pub fn place(&self, path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("a directory");
        std::fs::write(path, text).expect("placing a file");
    }

    /// The same, at the mode a file holding secrets is read at — the file's
    /// own and its directory's alike. Both checks run ahead of everything else
    /// a token read could refuse, so a case about anything else has to arrange
    /// modes that get past them.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging the modes a token file is read at.
    pub fn place_private(&self, path: &Path, text: &str) {
        use std::os::unix::fs::PermissionsExt;
        self.place(path, text);
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .expect("tightening a mode");
        std::fs::set_permissions(
            path.parent().expect("a directory"),
            std::fs::Permissions::from_mode(0o700),
        )
        .expect("tightening a directory mode");
    }

    /// The bytes at `path`.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: reading what the API wrote.
    pub fn text_at(&self, path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
    }

    /// Every entry name directly inside `directory`.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: judging what the write protocol left behind.
    pub fn names_in(&self, directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .expect("a directory")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }
}

impl Drop for Scratch {
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: removing the tree this test made.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A vault name, or a panic naming what was wrong with it.
pub fn name(text: &str) -> VaultName {
    VaultName::new(text).unwrap_or_else(|problem| panic!("`{text}` is a vault name: {problem}"))
}

/// A vault root, or a panic naming what was wrong with it.
pub fn root(text: &str) -> VaultRoot {
    VaultRoot::new(text).unwrap_or_else(|problem| panic!("`{text}` is a vault root: {problem}"))
}

/// An entry with the two fields that have no default.
pub fn entry(vault: &str, at: &str) -> Entry {
    Entry::new(name(vault), root(at))
}

/// A token label, or a panic naming what was wrong with it.
pub fn label(text: &str) -> TokenLabel {
    TokenLabel::new(text).unwrap_or_else(|problem| panic!("`{text}` is a token label: {problem}"))
}

/// The registry surface, reached through one pair of items.
///
/// `norn_config::registry::read` and `mutate` are a boundary rule's subject:
/// the serving set is `norn-host`'s to read and to change, and every other use
/// carves itself out at the use site. This crate owns the surface, so its own
/// suite is a carve-out at every case that touches a registry — and a case file
/// full of them reaches for one allow over the whole file, which is the shape
/// that does damage: `disallowed_methods` carries the filesystem rule too, so an
/// allow wide enough to cover a file retires the `std::fs` markers standing in
/// it. Cases reach the surface through here instead, and the marker sits on
/// these two items rather than on the file.
pub mod registry {
    use norn_config::registry::Registry;
    use norn_config::{ConfigDirs, ConfigError};

    #[allow(clippy::disallowed_methods)] // The registry surface is this crate's own; here it is what a case reads.
    pub fn read(dirs: &ConfigDirs) -> Result<Registry, ConfigError> {
        norn_config::registry::read(dirs)
    }

    #[allow(clippy::disallowed_methods)] // The registry surface is this crate's own; here it is what a case writes through.
    pub fn mutate<T>(
        dirs: &ConfigDirs,
        apply: impl FnOnce(&mut Registry) -> Result<T, ConfigError>,
    ) -> Result<T, ConfigError> {
        norn_config::registry::mutate(dirs, apply)
    }
}
