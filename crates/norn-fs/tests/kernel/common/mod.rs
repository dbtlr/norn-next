//! A scratch vault, its shadow home, and the arrangements a case makes in it.
//!
//! Every case works over a [`Scratch`]: a vault root and a data directory under
//! one temporary tree, both removed when the case ends. The tree is under the
//! system temporary directory, so both sit on one filesystem and the shadow home
//! resolves to the data directory — which is the placement the contract wants
//! and the one a case should be exercising.

use std::path::{Path, PathBuf};

use norn_fs::{ContentHash, MaintainershipKey, Placement, ShadowHome};
use norn_testkit::scratch;

/// The maintainership one scratch tree's shadow home is keyed by.
pub fn key() -> MaintainershipKey {
    MaintainershipKey::new("norn-dev", "notes", "0123456789abcdef").expect("three path components")
}

/// A vault and its shadow home, for the length of one case.
///
/// The naming and the removal are [`scratch::Scratch`]'s; what this adds is
/// the vault root and the resolved shadow home a case works over.
pub struct Scratch {
    tree: scratch::Scratch,
    shadows: ShadowHome,
}

impl Scratch {
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the tree this case works over.
    pub fn new(label: &str) -> Scratch {
        let tree = scratch::Scratch::new(&format!("norn-fs-{label}"));
        std::fs::create_dir_all(tree.join("vault")).expect("a vault root");
        let shadows = ShadowHome::resolve(
            &tree.join("vault"),
            &tree.join("data/vaults/notes/tmp"),
            &key(),
        )
        .expect("a shadow home");
        assert_eq!(
            shadows.placement(),
            Placement::DataRoot,
            "the scratch tree straddles two filesystems, so no case here is \
             exercising the placement the contract wants"
        );
        Scratch { tree, shadows }
    }

    pub fn shadows(&self) -> &ShadowHome {
        &self.shadows
    }

    /// The vault root.
    pub fn vault(&self) -> PathBuf {
        self.tree.join("vault")
    }

    /// A path inside the vault. Nothing is created.
    pub fn at(&self, relative: &str) -> PathBuf {
        self.vault().join(relative)
    }

    /// A directory inside the vault.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging the tree a case needs.
    pub fn directory(&self, relative: &str) -> PathBuf {
        let path = self.at(relative);
        std::fs::create_dir_all(&path).expect("a directory");
        path
    }

    /// Put `content` at a path inside the vault, the way something other than
    /// this crate would: a plain write, with no protocol around it.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging the bytes a case reads.
    pub fn place(&self, relative: &str, content: &[u8]) -> PathBuf {
        let path = self.at(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a directory");
        }
        std::fs::write(&path, content).expect("placing a document");
        path
    }

    /// Every entry name in the shadow home, sorted.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: judging what the protocol left behind.
    pub fn shadow_names(&self) -> Vec<String> {
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

/// Every entry name directly inside `directory`, sorted.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: judging what a write left in a directory.
pub fn names_in(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .unwrap_or_else(|e| panic!("reading {}: {e}", directory.display()))
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

/// The bytes at `path`.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: reading what the kernel wrote.
pub fn bytes_at(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// Whether anything at all is at `path`, link or file.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: judging what a case left behind.
pub fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// The `(device, inode)` pair `path` resolves to.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: judging which file a name means.
pub fn identity_at(path: &Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("reading the identity of {}: {e}", path.display()));
    (metadata.dev(), metadata.ino())
}

/// When `path` was last modified, as the filesystem records it now.
///
/// A fresh look rather than the value a write reported, so an outcome's `mtime`
/// is judged against the filesystem instead of against itself.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: judging what a write reported.
pub fn mtime_at(path: &Path) -> std::time::SystemTime {
    std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("reading the identity of {}: {e}", path.display()))
        .modified()
        .unwrap_or_else(|e| panic!("reading the mtime of {}: {e}", path.display()))
}

/// The permission bits at `path`.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: judging the mode a write published.
pub fn mode_at(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("reading the mode of {}: {e}", path.display()));
    metadata.permissions().mode() & 0o7777
}

/// Set the permission bits at `path`.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging the modes a case needs.
pub fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .unwrap_or_else(|e| panic!("setting the mode of {}: {e}", path.display()));
}

/// A second name for the file at `target`, sharing its inode.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging a foreign hard link.
pub fn hard_link(target: &Path, at: &Path) {
    std::fs::hard_link(target, at)
        .unwrap_or_else(|e| panic!("linking {} to {}: {e}", at.display(), target.display()));
}

/// A symbolic link at `at` naming `target`.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging a symbolic link.
pub fn symlink(target: &Path, at: &Path) {
    std::os::unix::fs::symlink(target, at)
        .unwrap_or_else(|e| panic!("linking {} to {}: {e}", at.display(), target.display()));
}

/// The hash of `content`, which is what a caller composes against.
pub fn hash(content: &[u8]) -> ContentHash {
    ContentHash::of(content)
}

/// Refuse to draw a conclusion from a mode this process is above.
///
/// The `EACCES` cases arrange a directory nothing may write into and expect the
/// operating system to enforce it. A privileged process is not subject to that,
/// so it cannot construct the case at all — and a bar that quietly passes
/// because the refusal it wanted was never going to happen is worse than one
/// that says so.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: probing whether a mode is enforced.
pub fn demand_unwritable(directory: &Path) {
    let probe = directory.join("norn-fs-permission-probe");
    if std::fs::write(&probe, b"").is_ok() {
        let _ = std::fs::remove_file(&probe);
        panic!(
            "{} is not writable and this process wrote into it anyway, so permission bits do not \
             apply here and the refusal this case is about cannot be constructed",
            directory.display()
        );
    }
}

/// The same, for a file nothing may read.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: probing whether a mode is enforced.
pub fn demand_unreadable(path: &Path) {
    if std::fs::read(path).is_ok() {
        panic!(
            "{} is not readable and this process read it anyway, so permission bits do not apply \
             here and the refusal this case is about cannot be constructed",
            path.display()
        );
    }
}
