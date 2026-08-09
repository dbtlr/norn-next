//! A temporary tree, its vault and its shadow home, for the length of one
//! in-crate test.
//!
//! One harness for every module's own suite. Three of them existed because three
//! modules each needed a directory that goes away; what they needed differed only
//! in which paths they asked for, which is a method rather than a type.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::shadow::{MaintainershipKey, Placement, ShadowHome};

/// Distinguishes two scratch trees taken in the same process.
static SERIAL: AtomicU64 = AtomicU64::new(0);

/// The maintainership one scratch tree's home is keyed by. One key per tree is
/// all a case needs; the cases about two keys spell their own.
pub(crate) fn key() -> MaintainershipKey {
    MaintainershipKey::new("norn-dev", "notes", "0123456789abcdef").expect("three path components")
}

/// A tree under the system temporary directory, removed when this is dropped.
pub(crate) struct Scratch {
    root: PathBuf,
    shadows: ShadowHome,
}

impl Scratch {
    /// A tree holding a vault root and a resolved shadow home.
    ///
    /// Both sit under one temporary directory and therefore on one filesystem, so
    /// the home resolves to the data directory — which is the placement the
    /// contract wants and the one a case should be exercising.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the tree this test works over.
    pub(crate) fn new(label: &str) -> Scratch {
        let root = std::env::temp_dir().join(format!(
            "norn-fs-{label}-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("vault")).expect("a vault root");
        let shadows = ShadowHome::resolve(
            &root.join("vault"),
            &root.join("data/vaults/notes/tmp"),
            &key(),
        )
        .expect("a shadow home");
        assert_eq!(
            shadows.placement(),
            Placement::DataRoot,
            "the scratch tree straddles two filesystems, so no case here is \
             exercising the placement the contract wants"
        );
        Scratch { root, shadows }
    }

    /// The shadow home writes in this tree stage into.
    pub(crate) fn shadows(&self) -> &ShadowHome {
        &self.shadows
    }

    /// A path anywhere under the tree. Nothing is created.
    pub(crate) fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// A path inside the vault. Nothing is created.
    pub(crate) fn at(&self, relative: &str) -> PathBuf {
        self.root.join("vault").join(relative)
    }

    /// A directory under the tree.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging the tree a test needs.
    pub(crate) fn directory(&self, relative: &str) -> PathBuf {
        let path = self.path(relative);
        std::fs::create_dir_all(&path).expect("a directory");
        path
    }

    /// Put `content` at a path inside the vault, the way something other than
    /// this crate would: a plain write, with no protocol around it.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging the bytes a test reads.
    pub(crate) fn place(&self, relative: &str, content: &[u8]) -> PathBuf {
        let path = self.at(relative);
        std::fs::write(&path, content).expect("placing a document");
        path
    }

    /// The bytes at `path`.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: reading what the kernel wrote.
    pub(crate) fn read(&self, path: &Path) -> Vec<u8> {
        std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
    }

    /// Whether anything at all is at `path`.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: judging what a test left behind.
    pub(crate) fn exists(&self, path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok()
    }

    /// The permission bits at `path`.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: judging the mode a write set.
    pub(crate) fn mode_at(&self, path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .unwrap_or_else(|e| panic!("reading the mode of {}: {e}", path.display()))
            .permissions()
            .mode()
            & 0o7777
    }

    /// Set the permission bits at `path`.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging the mode a test needs.
    pub(crate) fn set_mode(&self, path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .unwrap_or_else(|e| panic!("setting the mode of {}: {e}", path.display()));
    }

    /// Every entry name in the shadow home, sorted.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: judging what the protocol left behind.
    pub(crate) fn shadow_names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.shadows.directory())
            .expect("the shadow home")
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
