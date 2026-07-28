//! Reading a directory tree back off disk, in one fixed order.
//!
//! Both consumers — the tree digest and the calibration probe — need the same
//! thing: every node under a root, named by a forward-slash relative path, in
//! an order that does not depend on the filesystem's readdir order. The sort
//! is what makes the digest a property of the tree rather than of the machine.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// One node of a walked tree.
pub struct Node {
    /// Forward-slash path relative to the walk root.
    pub rel: String,
    /// The absolute (or root-relative) path to read.
    pub path: PathBuf,
    pub is_dir: bool,
}

/// Every node under `root`, sorted by relative path. Symbolic links are
/// reported by their own metadata and never followed: a walk that follows
/// links can revisit a subtree, and a generated tree contains none.
pub fn walk(root: &Path) -> io::Result<Vec<Node>> {
    let mut out = Vec::new();
    collect(root, "", &mut out)?;
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

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
        let is_dir = entry.file_type()?.is_dir();
        if is_dir {
            collect(&path, &rel, out)?;
        }
        out.push(Node { rel, path, is_dir });
    }
    Ok(())
}
