//! The suite-manifest digest: one value over everything that decides what a
//! certification run *is*.
//!
//! A run's result means something only against the suite that produced it. Five
//! consecutive qualifying runs of a suite that changed underneath them is one
//! run five times over, so the campaign needs a value that moves whenever the
//! thing being run moves — and does not move for a run that only happened at a
//! different hour on a different machine.
//!
//! # What it closes over
//!
//! The lanes ([`MANIFEST_TREES`]), the toolchain and the resolved dependency
//! graph, the inventory of required cases, the comparator every convergence
//! verdict is read off, the injected-failure code every rung-2 and rung-3 case
//! is reached through, and the files that hold the authored bounds. Each entry
//! in [`MANIFEST_FILES`] says why it is in, because an entry nobody can justify
//! is one a later edit will drop.
//!
//! # What it does not close over
//!
//! The product's own source. That is what the candidate SHA is for, and folding
//! it in here would make every commit a new suite and the five-run count
//! unreachable by construction. The two values are recorded side by side in the
//! ledger and answer different questions: the SHA says what was certified, and
//! this says what certified it.
//!
//! # Determinism
//!
//! Files are hashed in sorted path order, each path and each content framed
//! behind its own length, and nothing timestamped or environment-derived takes
//! part. The value is therefore reproducible from a clean checkout of the same
//! commit on any machine — which is what makes it a thing two runs can be
//! compared by.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use norn_fixtures::digest::{Sha256, hex};

use super::inventory;

/// A directory whose every file takes part, walked recursively.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManifestTree {
    pub path: &'static str,
    pub why: &'static str,
}

/// One file that takes part.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManifestFile {
    pub path: &'static str,
    pub why: &'static str,
}

/// The lane definitions, whole. A workflow file or a lane script added, edited
/// or deleted is a change to what a run does, whichever file it lands in — so
/// these are swept rather than listed, and a new workflow joins the manifest by
/// existing.
pub const MANIFEST_TREES: &[ManifestTree] = &[
    ManifestTree {
        path: ".github/workflows",
        why: "the lanes themselves: which suites run, under which features, on which runners, \
              with which timeouts",
    },
    ManifestTree {
        path: ".github/scripts",
        why: "what a lane step actually invokes, and what it accepts as a passing result line",
    },
];

/// The files outside the swept trees that decide what a run is.
pub const MANIFEST_FILES: &[ManifestFile] = &[
    ManifestFile {
        path: "rust-toolchain.toml",
        why: "the compiler and the lint set every suite is built with",
    },
    ManifestFile {
        path: "Cargo.lock",
        why: "the resolved dependency graph, SQLite's own version included — which is what a work \
              bar's step counts are measured against",
    },
    ManifestFile {
        path: "crates/norn-testkit/src/certification/inventory.rs",
        why: "the inventory of required cases, folded in as a file as well as through its own \
              digest, so an edit to the reconciliation beside the table moves the value too",
    },
    ManifestFile {
        path: "crates/norn-testkit/src/equivalence.rs",
        why: "the comparator every convergence verdict and the operational-validity leg are read \
              off",
    },
    ManifestFile {
        path: "crates/norn-testkit/src/churn.rs",
        why: "the seeded workload families the churn suite applies to a live tree",
    },
    ManifestFile {
        path: "crates/norn-testkit/src/work.rs",
        why: "the shape a work bar is stated in",
    },
    ManifestFile {
        path: "crates/norn-fs/src/faults.rs",
        why: "the write protocol's fault seam: the stages a publication can be failed at and how",
    },
    ManifestFile {
        path: "crates/norn-store/src/store.rs",
        why: "the store's induced-failure module and the arms it fires at — the whole file, \
              because the seam is a module inside it and a rung's behaviour is the rest of it",
    },
    ManifestFile {
        path: "crates/norn-host/tests/churn.rs",
        why: "the churn suite, whose authored cost bounds and settle budgets are the bars a \
              qualifying run passes under",
    },
    ManifestFile {
        path: "crates/norn-store/tests/store/pillars.rs",
        why: "the work bar's authored floor and coefficient",
    },
];

/// What went wrong computing a digest.
#[derive(Debug)]
pub enum ManifestError {
    /// A path the manifest names is not in the checkout. The manifest is a
    /// claim about what decides a run, so a missing entry is a stale claim
    /// rather than a file to skip.
    Missing(PathBuf),
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::Missing(path) => write!(
                f,
                "the suite manifest names `{}`, which is not in this checkout. A manifest entry \
                 that no longer exists is a stale claim about what decides a run: remove the \
                 entry in the same diff that removed the file.",
                path.display()
            ),
            ManifestError::Read { path, source } => {
                write!(f, "could not read `{}`: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for ManifestError {}

/// **The suite-manifest digest.**
///
/// Every named file's bytes, in sorted workspace-relative path order, each path
/// and each content framed behind its own length so no two different manifests
/// run together into the same value. The inventory's own contract digest goes in
/// beside them, so an obligation retitled or re-laned moves the value even where
/// the file's bytes would have been read the same way.
pub fn digest(workspace_root: &Path) -> Result<String, ManifestError> {
    let files = collect(workspace_root)?;

    let mut hasher = Sha256::new();
    hasher.update_framed(b"norn suite manifest v1");
    hasher.update_framed(inventory::contract_digest().as_bytes());
    hasher.update_framed(&(files.len() as u64).to_be_bytes());
    for (relative, bytes) in &files {
        hasher.update_framed(relative.as_bytes());
        hasher.update_framed(bytes);
    }
    Ok(hex(&hasher.finish()))
}

/// Every workspace-relative path the manifest covers, in sorted order.
///
/// Public because a reader asking "what moved the digest" needs the list, and a
/// list rebuilt by hand would be a second manifest.
pub fn covered_paths(workspace_root: &Path) -> Result<Vec<String>, ManifestError> {
    Ok(collect(workspace_root)?.into_keys().collect())
}

/// Every covered file's bytes, keyed by workspace-relative path.
///
/// A [`BTreeMap`] because sorted path order is the whole of the ordering rule:
/// two checkouts that hold the same files digest the same however a directory
/// walk happened to enumerate them.
fn collect(workspace_root: &Path) -> Result<BTreeMap<String, Vec<u8>>, ManifestError> {
    let mut found = BTreeMap::new();
    for tree in MANIFEST_TREES {
        let root = workspace_root.join(tree.path);
        if !is_directory(&root) {
            return Err(ManifestError::Missing(PathBuf::from(tree.path)));
        }
        walk(workspace_root, &root, &mut found)?;
    }
    for file in MANIFEST_FILES {
        let path = workspace_root.join(file.path);
        let bytes = read(&path)?;
        found.insert(file.path.to_string(), bytes);
    }
    Ok(found)
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: reads the workspace's own lane and suite definitions.
fn walk(
    workspace_root: &Path,
    directory: &Path,
    found: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), ManifestError> {
    let entries = std::fs::read_dir(directory).map_err(|source| ManifestError::Read {
        path: directory.to_path_buf(),
        source,
    })?;
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ManifestError::Read {
            path: directory.to_path_buf(),
            source,
        })?;
        paths.push(entry.path());
    }
    paths.sort();
    for path in paths {
        if is_directory(&path) {
            walk(workspace_root, &path, found)?;
            continue;
        }
        let relative = path
            .strip_prefix(workspace_root)
            .map(|relative| relative.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| path.to_string_lossy().to_string());
        let bytes = read(&path)?;
        found.insert(relative, bytes);
    }
    Ok(())
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: reads the workspace's own lane and suite definitions.
fn is_directory(path: &Path) -> bool {
    path.is_dir()
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: reads the workspace's own lane and suite definitions.
fn read(path: &Path) -> Result<Vec<u8>, ManifestError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            Err(ManifestError::Missing(path.to_path_buf()))
        }
        Err(source) => Err(ManifestError::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{MANIFEST_FILES, MANIFEST_TREES};
    use std::collections::BTreeSet;

    /// Every entry says why it is in the manifest. An entry with no reason is
    /// one a later edit deletes without knowing what it changed.
    #[test]
    fn every_manifest_entry_states_why_it_is_in() {
        for tree in MANIFEST_TREES {
            assert!(
                tree.why.trim().len() > 20,
                "`{}` says too little",
                tree.path
            );
        }
        for file in MANIFEST_FILES {
            assert!(
                file.why.trim().len() > 20,
                "`{}` says too little",
                file.path
            );
        }
    }

    /// No path is named twice, and no named file sits inside a swept tree —
    /// either would hash one file twice under two keys and make the manifest's
    /// own list a poor answer to "what moved the digest".
    #[test]
    fn no_path_is_covered_twice() {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for tree in MANIFEST_TREES {
            assert!(seen.insert(tree.path), "`{}` is named twice", tree.path);
        }
        for file in MANIFEST_FILES {
            assert!(seen.insert(file.path), "`{}` is named twice", file.path);
            for tree in MANIFEST_TREES {
                assert!(
                    !file.path.starts_with(&format!("{}/", tree.path)),
                    "`{}` is inside the swept tree `{}`",
                    file.path,
                    tree.path
                );
            }
        }
    }
}
