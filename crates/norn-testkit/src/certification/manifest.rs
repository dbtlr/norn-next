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
//! graph, the inventory of required cases, the qualification rules the inventory
//! is reconciled and a record is judged by, the comparator every convergence
//! verdict is read off, the instrument a work reading is taken through, the
//! injected-failure seams every rung-2 and rung-3 case is reached through, every
//! certification suite's own source, and the recorded baselines a measurement
//! step is compared against. Each entry in [`MANIFEST_FILES`] says why it is in,
//! because an entry nobody can justify is one a later edit will drop.
//!
//! **The rules are inside the value they decide.** This file and [`ledger`] are
//! entries like any other, hashed as bytes off the checkout. That is what closes
//! the self-reference: the digest is a function of file contents rather than of
//! the constants a build compiled, so a rule loosened moves the value it is the
//! rule for, and computing it terminates.
//!
//! [`ledger`]: super::ledger
//!
//! # What it does not close over
//!
//! **The product's own source.** That is what the candidate SHA is for, and
//! folding it in here would make every commit a new suite and the five-run count
//! unreachable by construction. The two values are recorded side by side in the
//! ledger and answer different questions: the SHA says what was certified, and
//! this says what certified it. A file that holds both — a fault seam inside a
//! product module — is a file to split rather than a reason to widen this list.
//!
//! **The runner image's own version.** A lane names a runner label, and the
//! label is in this digest because it is workflow text; the image that label
//! resolved to on the day is not. Two runs a month apart under `ubuntu-latest`
//! ran on different images, with different kernels and different filesystem
//! behaviour, and agree on this value. What carries that is the record rather
//! than the digest: [`super::ledger::Platform`] holds the runner, the OS, the
//! architecture, the watcher backend and the volume's case answer, so a campaign
//! comparing five records sees the machine change even where the suite did not.
//! **Neither answers for the image's patch level**, and nothing here should be
//! read as saying two runs met the same kernel.
//!
//! # Determinism
//!
//! Files are hashed in sorted path order, each path and each content framed
//! behind its own length, and nothing timestamped or environment-derived takes
//! part. The value is therefore reproducible from a checkout of the same commit
//! on any machine — which is what makes it a thing two runs can be compared by.
//!
//! **It is a reading of the working tree, not of the commit.** Every byte comes
//! off the files as they sit on disk, so an uncommitted edit to a covered file
//! moves the digest and a run over a dirty checkout records a value no other
//! checkout of that commit reproduces. A qualifying run is therefore a run over
//! a clean checkout of the candidate it names — which is what a workflow's own
//! `actions/checkout` gives it, and what a local run reproducing a recorded
//! value has to arrange for itself.

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
        path: "crates/norn-testkit/src/certification/manifest.rs",
        why: "this list and the rule that folds it: what a run is certified against is decided \
              here, so a value computed by a different rule is a different value's namesake. \
              Digested as bytes off the checkout like every other entry, which is what makes the \
              self-reference terminate rather than recur",
    },
    ManifestFile {
        path: "crates/norn-testkit/src/certification/ledger.rs",
        why: "what makes a record qualifying: the classification a run's contents imply and the \
              validator a campaign counts through. A run counted under a looser rule is not the \
              same run",
    },
    ManifestFile {
        path: "crates/norn-testkit/src/regression.rs",
        why: "the reconciliation engine the inventory is walked by — how cargo is asked what \
              compiled, and what a reference is held to resolve to",
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
        why: "the shape a work bar is stated in, and the row count arithmetic a caller states one \
              against",
    },
    ManifestFile {
        path: "crates/norn-fs/src/faults.rs",
        why: "the write protocol's fault seam: the stages a publication can be failed at and how",
    },
    ManifestFile {
        path: "crates/norn-store/src/faults.rs",
        why: "the store's fault seam: the arms a rung-2 or rung-3 case is reached through, and \
              the points the store's own paths read them at",
    },
    ManifestFile {
        path: "crates/norn-store/src/request.rs",
        why: "the instrument the work bar reads — the step count a drain is judged by and the \
              plan a statement reports — so a reading taken through a different instrument is a \
              different reading",
    },
    // The certification suites themselves. Every case a qualifying run
    // executes is authored in one of these files, so an assertion loosened is
    // a run that certified less: the id and the carrier would not move, and
    // the reconciliation would not either.
    ManifestFile {
        path: "crates/norn-host/tests/churn.rs",
        why: "the churn suite, whose authored cost bounds and settle budgets are the bars a \
              qualifying run passes under",
    },
    ManifestFile {
        path: "crates/norn-host/tests/equivalence.rs",
        why: "the operational leg: one vault derived twice from zero, which is what says the \
              comparator reports agreement where agreement is what there is",
    },
    ManifestFile {
        path: "crates/norn-host/tests/lockdown.rs",
        why: "the host's induced-failure suite — the rung-2 and rung-3 cases a full disk, an \
              unreadable document, a revoked subtree and a tear are asserted through",
    },
    ManifestFile {
        path: "crates/norn-host/tests/coverage.rs",
        why: "the one trust transition met at the production path with no seam: what a host does \
              with an entry whose vault root a real backend stopped covering",
    },
    ManifestFile {
        path: "crates/norn-host/tests/kill_recovery.rs",
        why: "the crash-convergence case: what the next attach after a process died mid-increment \
              is required to converge on",
    },
    ManifestFile {
        path: "crates/norn-fs/tests/lockdown.rs",
        why: "the write kernel's induced-failure suite — the publication checkpoints a tear is \
              required to leave one whole document at",
    },
    ManifestFile {
        path: "crates/norn-store/tests/environment.rs",
        why: "the store's induced-failure suite — what separates a hostile environment from \
              damaged state, which is what decides whether a rung runs at all",
    },
    ManifestFile {
        path: "crates/norn-store/tests/store/pillars.rs",
        why: "the work bar's authored floor, coefficient and row count",
    },
    // The recorded baselines. A measurement lane compares against these rather
    // than against a history, so a baseline edited is a bar moved.
    ManifestFile {
        path: "crates/norn-fixtures/tests/baselines/mod.rs",
        why: "the fixture generator's recorded readings, which are the bars its lane steps compare \
              against",
    },
    ManifestFile {
        path: "crates/norn-host/tests/baselines/mod.rs",
        why: "the host's recorded readings — the attach memory ceilings and ratios a lane step \
              fails against",
    },
    ManifestFile {
        path: "crates/norn-text/tests/baselines/mod.rs",
        why: "the parser's recorded readings, which the frontmatter cost step is judged against",
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
