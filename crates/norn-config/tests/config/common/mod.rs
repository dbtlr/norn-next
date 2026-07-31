//! Scratch machines, and the shapes the suite writes into them.
//!
//! Every case here works over a [`Scratch`]: two temporary base directories
//! standing in for the XDG bases, and the [`ConfigDirs`] built over them. No
//! case reads or sets an environment variable — that is the one entry point
//! the environment suite exercises, and it lives in its own binary so that
//! nothing here runs beside a process-wide write.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
        let dirs = ConfigDirs::new(root.join("config"), root.join("data"));
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

    /// Put `text` at `path`, making its directory first. Used to arrange a
    /// file the API would never write — a version ahead of this build, a
    /// document with fields it does not model, bytes that are not TOML.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging bytes the API refuses to write.
    pub fn place(&self, path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("a directory");
        std::fs::write(path, text).expect("placing a file");
    }

    /// The same, at the mode a file holding secrets is read at. The token
    /// file's permission check runs ahead of everything else it could refuse,
    /// so a case about anything else has to arrange a mode that gets past it.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging the mode a token file is read at.
    pub fn place_private(&self, path: &Path, text: &str) {
        use std::os::unix::fs::PermissionsExt;
        self.place(path, text);
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .expect("tightening a mode");
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
