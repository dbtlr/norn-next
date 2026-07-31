//! Scratch trees for the integration suites.
//!
//! Trees are generated under cargo's per-package temporary directory and
//! removed as soon as the value being asserted has been extracted from them.
//! Two properties matter and are easy to lose: the directory name is never
//! dot-prefixed, because a hidden root is exactly what a vault walk is
//! entitled to skip; and every generation gets its own name, because the
//! suites run in parallel.
//!
//! Each integration binary compiles the whole module and uses part of it, so
//! the unused remainder is expected rather than a defect.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use norn_fixtures::probe::VaultStats;
use norn_fixtures::{Manifest, Profile, generate, probe};

/// What a walked entry is, as the directory reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    Symlink,
}

/// One entry of a generated tree.
pub struct Entry {
    /// Forward-slash path relative to the tree root.
    pub rel: String,
    pub path: PathBuf,
    pub kind: Kind,
}

impl Entry {
    /// Whether this is a Markdown document — a regular file named `.md`.
    ///
    /// A symbolic link named `.md` is not one, however it resolves: counting
    /// it as a document counts one document twice, and reading through one
    /// whose target is gone fails outright. Every suite here asks this
    /// question through this one predicate.
    pub fn is_document(&self) -> bool {
        self.kind == Kind::File && self.rel.ends_with(".md")
    }

    /// The entry's own name, without the directories above it.
    pub fn name(&self) -> &str {
        self.rel.rsplit('/').next().unwrap_or(&self.rel)
    }

    /// The directory holding this entry, as a relative path. The tree root is
    /// the empty string.
    pub fn parent(&self) -> &str {
        match self.rel.rsplit_once('/') {
            Some((parent, _)) => parent,
            None => "",
        }
    }
}

/// Every entry under `root`, sorted by relative path, **never following a
/// symbolic link**.
#[allow(clippy::disallowed_methods)] // Walks the generated tree the suites measure.
pub fn walk(root: &Path) -> Vec<Entry> {
    fn collect(dir: &Path, prefix: &str, found: &mut Vec<Entry>) {
        for entry in fs::read_dir(dir).expect("reading a generated directory") {
            let entry = entry.expect("reading a generated directory entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            // `DirEntry::file_type` describes the entry itself, so a link is a
            // link whatever it names.
            let file_type = entry.file_type().expect("a directory entry's type");
            let kind = if file_type.is_symlink() {
                Kind::Symlink
            } else if file_type.is_dir() {
                Kind::Dir
            } else {
                Kind::File
            };
            let path = entry.path();
            if kind == Kind::Dir {
                collect(&path, &rel, found);
            }
            found.push(Entry { rel, path, kind });
        }
    }

    let mut found = Vec::new();
    collect(root, "", &mut found);
    found.sort_by(|a, b| a.rel.cmp(&b.rel));
    found
}

/// Every Markdown document under `root`.
pub fn documents(root: &Path) -> Vec<Entry> {
    walk(root).into_iter().filter(Entry::is_document).collect()
}

/// The text of every Markdown document under `root`.
#[allow(clippy::disallowed_methods)] // Reads the generated documents the suites count in.
pub fn document_texts(root: &Path) -> Vec<String> {
    documents(root)
        .iter()
        .map(|entry| fs::read_to_string(&entry.path).expect("reading a generated document"))
        .collect()
}

/// Every symbolic link under `root`, with the target as it was written.
#[allow(clippy::disallowed_methods)] // Reads the targets of the links the suites classify.
pub fn symlinks(root: &Path) -> Vec<(Entry, String)> {
    walk(root)
        .into_iter()
        .filter(|entry| entry.kind == Kind::Symlink)
        .map(|entry| {
            let target = fs::read_link(&entry.path)
                .expect("reading a generated link")
                .to_string_lossy()
                .into_owned();
            (entry, target)
        })
        .collect()
}

/// A fresh, empty, non-hidden directory named `label`.
#[allow(clippy::disallowed_methods)] // Scratch trees are this module's to make and remove.
pub fn fresh(label: &str) -> PathBuf {
    assert!(
        !label.starts_with('.'),
        "a scratch root must not be hidden: {label}"
    );
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(label);
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("clearing a stale scratch tree");
    }
    dir
}

/// Generate, take the contract digest, then delete the tree.
///
/// The digest comes off the manifest rather than off a walk of the result:
/// the contract is stated over what the generator emits, and a walk would key
/// on whatever spelling the filesystem hands back.
#[allow(clippy::disallowed_methods)] // Scratch trees are this module's to make and remove.
pub fn generate_and_digest(label: &str, profile: &Profile, seed: u64) -> ([u8; 32], Manifest) {
    let dir = fresh(label);
    let manifest = generate(profile, seed, &dir).expect("generating a scratch tree");
    fs::remove_dir_all(&dir).expect("removing a scratch tree");
    (manifest.tree_digest, manifest)
}

/// Generate, measure the tree's shape, then delete it.
#[allow(clippy::disallowed_methods)] // Scratch trees are this module's to make and remove.
pub fn generate_and_measure(label: &str, profile: &Profile, seed: u64) -> (VaultStats, Manifest) {
    let dir = fresh(label);
    let manifest = generate(profile, seed, &dir).expect("generating a scratch tree");
    let stats = probe::measure(&dir).expect("measuring a scratch tree");
    fs::remove_dir_all(&dir).expect("removing a scratch tree");
    (stats, manifest)
}

/// Generate, hand the tree to `inspect`, then delete it.
#[allow(clippy::disallowed_methods)] // Scratch trees are this module's to make and remove.
pub fn with_tree<T>(
    label: &str,
    profile: &Profile,
    seed: u64,
    inspect: impl FnOnce(&PathBuf, &Manifest) -> T,
) -> T {
    let dir = fresh(label);
    let manifest = generate(profile, seed, &dir).expect("generating a scratch tree");
    let out = inspect(&dir, &manifest);
    fs::remove_dir_all(&dir).expect("removing a scratch tree");
    out
}
