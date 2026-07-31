//! Reading a directory tree back off disk, in one fixed order.
//!
//! Every consumer needs the same thing: each node under a root, named by a
//! forward-slash relative path, in an order that does not depend on the
//! filesystem's readdir order. The sort is what makes the digest a property of
//! the tree rather than of the machine. The tree digest and the calibration
//! probe read it here, and so does anything outside the crate that reads a
//! generated tree back, so that "a symbolic link is not the thing it names" is
//! decided once.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// What a walked node is, as the directory reports it.
///
/// A symbolic link is its own kind rather than the kind of whatever it points
/// at: a link to a directory that counted as a directory would be descended
/// into twice, and a link to a document that counted as a document would count
/// one document twice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    Symlink,
}

/// One node of a walked tree.
pub struct Node {
    /// Forward-slash path relative to the walk root.
    pub rel: String,
    /// The absolute (or root-relative) path to read.
    pub path: PathBuf,
    pub kind: Kind,
}

/// Every node under `root`, sorted by relative path. Symbolic links are
/// reported by their own metadata and never followed: a walk that follows
/// links can revisit a subtree, leave the tree it was asked about, or fail on
/// a target that is not there.
pub fn walk(root: &Path) -> io::Result<Vec<Node>> {
    let mut out = Vec::new();
    collect(root, "", &mut out)?;
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

#[allow(clippy::disallowed_methods)] // The walk this crate measures generated trees with.
fn collect(dir: &Path, prefix: &str, out: &mut Vec<Node>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} holds a non-UTF-8 file name", dir.display()),
            ));
        };
        let rel = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        let path = entry.path();
        // `DirEntry::file_type` reports the entry itself, so a symbolic link
        // is a symbolic link whatever it names.
        let file_type = entry.file_type()?;
        let kind = if file_type.is_symlink() {
            Kind::Symlink
        } else if file_type.is_dir() {
            Kind::Dir
        } else {
            Kind::File
        };
        if kind == Kind::Dir {
            collect(&path, &rel, out)?;
        }
        out.push(Node { rel, path, kind });
    }
    Ok(())
}
