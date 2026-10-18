//! The Obsidian link probe: its case table, and the tree it ran against
//! installed with every name at the code points the table states.
//!
//! `tests/fixtures/obsidian-links` holds a vault folder an Obsidian install
//! opened and `cases.json`, which names every source and competitor in it at
//! its intended code points. Some are decomposed (NFD) on purpose, beside a
//! composed (NFC) twin, so a link spelled in one form meets a file named in
//! the other. `cases.json` is the baseline a recorded observation pins by
//! hash, so it is read here and never rewritten.
//!
//! **Git does not carry a decomposed name portably**: on macOS, where clones
//! precompose Unicode paths by default, a checked-in NFD name reads back
//! composed and the checked-in file shows as untracked. So the tree is
//! checked in at composed names, and `install.json` beside the case table
//! names each file whose installed spelling differs. [`Probe::install`] copies the
//! tree and makes those renames. It is the one way the tree reaches a vault:
//! the `norn-obsidian-probe` command installs it for a manual Obsidian run,
//! and a suite asserting against the recorded answers installs it the same
//! way.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The `cases.json` schema this loader reads.
pub const CASES_SCHEMA_VERSION: u32 = 1;

/// The `install.json` schema this loader reads.
pub const INSTALL_SCHEMA_VERSION: u32 = 1;

/// The probe's case table, its install renames, and the directory they were
/// read from.
#[derive(Debug)]
pub struct Probe {
    dir: PathBuf,
    table: Table,
    install: Install,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    schema_version: u32,
    probe_root: String,
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Install {
    schema_version: u32,
    renamed_on_install: Vec<Rename>,
}

/// One recorded case: the document holding a link, the link as written, and
/// the competitors the setup placed for it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub concern: String,
    /// The vault path of the document holding the link.
    pub source: String,
    /// The link as the source spells it.
    pub link: String,
    /// The vault paths the setup placed as competitors, not the expected
    /// destination.
    pub candidates: Vec<String>,
}

/// A file checked in at one vault path and installed at another: the same
/// name in another Unicode form.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Rename {
    pub checked_in: String,
    pub installed: String,
}

impl Probe {
    /// The probe this workspace checks in.
    pub fn workspace() -> Result<Probe, String> {
        Probe::load(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/obsidian-links"),
        )
    }

    /// The probe in `dir`: its `cases.json`, its `install.json`, and the
    /// `vault/` tree beside them.
    pub fn load(dir: &Path) -> Result<Probe, String> {
        let table: Table = read_json(
            &dir.join("cases.json"),
            CASES_SCHEMA_VERSION,
            |table: &Table| table.schema_version,
        )?;
        let install: Install = read_json(
            &dir.join("install.json"),
            INSTALL_SCHEMA_VERSION,
            |install: &Install| install.schema_version,
        )?;
        Ok(Probe {
            dir: dir.to_path_buf(),
            table,
            install,
        })
    }

    /// The vault-relative folder every probe path lies under.
    pub fn probe_root(&self) -> &str {
        &self.table.probe_root
    }

    pub fn cases(&self) -> &[Case] {
        &self.table.cases
    }

    pub fn renamed_on_install(&self) -> &[Rename] {
        &self.install.renamed_on_install
    }

    /// Install the probe tree into the vault rooted at `vault`: every file
    /// under the probe's `vault/` lands at the same relative path, and each
    /// [`Rename`] moves its file to its installed spelling.
    ///
    /// **A probe lands fresh.** Its root inside `vault` must not already
    /// stand, because results read against a tree that kept another run's
    /// edits would describe neither run; the install refuses rather than
    /// merging. Only directories and regular files are copied, so a tree that
    /// gained a link or a special file refuses too.
    ///
    /// **A failed install leaves no probe behind.** Everything it writes is
    /// under the probe's root, which it created itself, so it removes that
    /// root on failure and the same install can run again. The folders above
    /// the root are made where missing and kept.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: installing the probe tree a person or a suite names.
    pub fn install(&self, vault: &Path) -> Result<(), String> {
        let root = vault.join(&self.table.probe_root);
        // Claiming the root with one create is the check: a second install
        // into the same vault, or anything already standing there, a link
        // included, refuses here rather than sharing a root whose cleanup
        // would remove the other's tree.
        if let Some(parent) = root.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("creating {}: {error}", parent.display()))?;
        }
        match fs::create_dir(&root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(format!(
                    "{} already holds a probe; install into a vault without one",
                    root.display()
                ));
            }
            Err(error) => return Err(format!("creating {}: {error}", root.display())),
        }
        let installed = copy_tree(&self.dir.join("vault").join(&self.table.probe_root), &root)
            .and_then(|()| {
                self.renamed_on_install().iter().try_for_each(|rename| {
                    let from = vault.join(&rename.checked_in);
                    let to = vault.join(&rename.installed);
                    fs::rename(&from, &to).map_err(|error| {
                        format!("renaming {} to {}: {error}", from.display(), to.display())
                    })
                })
            });
        installed.map_err(|error| match fs::remove_dir_all(&root) {
            Ok(()) => format!("{error}; the partial probe was removed"),
            Err(removal) => format!(
                "{error}; removing the partial probe at {} failed too: {removal}",
                root.display()
            ),
        })
    }
}

/// The JSON file at `path`, refused unless `version` reads `expected` from it.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: the probe's own data files.
fn read_json<T: serde::de::DeserializeOwned>(
    path: &Path,
    expected: u32,
    version: impl Fn(&T) -> u32,
) -> Result<T, String> {
    let text =
        fs::read_to_string(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    let value: T = serde_json::from_str(&text)
        .map_err(|error| format!("parsing {}: {error}", path.display()))?;
    if version(&value) != expected {
        return Err(format!(
            "{} is schema version {}, and this loader reads {expected}",
            path.display(),
            version(&value)
        ));
    }
    Ok(value)
}

/// Copy every directory and regular file under `from` to the same relative
/// path under `to`, refusing any other kind of entry.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: copying the probe tree.
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|error| format!("creating {}: {error}", to.display()))?;
    let entries =
        fs::read_dir(from).map_err(|error| format!("reading {}: {error}", from.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("reading {}: {error}", from.display()))?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let kind = entry
            .file_type()
            .map_err(|error| format!("reading {}: {error}", source.display()))?;
        if kind.is_dir() {
            copy_tree(&source, &target)?;
        } else if kind.is_file() {
            fs::copy(&source, &target).map_err(|error| {
                format!(
                    "copying {} to {}: {error}",
                    source.display(),
                    target.display()
                )
            })?;
        } else {
            return Err(format!(
                "{} is neither a directory nor a regular file",
                source.display()
            ));
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::collections::BTreeSet;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    use super::*;
    use crate::scratch::Scratch;

    /// Every vault path the cases name, at the code points they name it.
    fn named_paths(probe: &Probe) -> BTreeSet<String> {
        probe
            .cases()
            .iter()
            .flat_map(|case| std::iter::once(&case.source).chain(&case.candidates))
            .cloned()
            .collect()
    }

    /// How many regular files stand anywhere under `dir`.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: counting the checked-in tree.
    fn files_under(dir: &Path) -> usize {
        fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("reading {}: {error}", dir.display()))
            .map(|entry| entry.expect("an entry").path())
            .map(|path| if path.is_dir() { files_under(&path) } else { 1 })
            .sum()
    }

    /// Whether the folder holding `path` under `vault` lists its last name
    /// with exactly these bytes.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: reading the installed tree's names.
    fn listed_exactly(vault: &Path, path: &str) -> bool {
        let path = Path::new(path);
        let parent = vault.join(path.parent().expect("a probe path has a folder"));
        let name = path.file_name().expect("a probe path has a name");
        fs::read_dir(&parent)
            .unwrap_or_else(|error| panic!("reading {}: {error}", parent.display()))
            .map(|entry| entry.expect("an entry").file_name())
            .any(|listed| OsStr::new(&listed).as_bytes() == name.as_bytes())
    }

    /// **An installed probe names every case path at the table's own code
    /// points**: each source and competitor is listed byte for byte as the
    /// table spells it, so a decomposed name stands decomposed beside its
    /// composed twin, whatever form git checked the tree out in.
    #[test]
    fn an_installed_probe_lists_every_case_path_at_its_stated_code_points() {
        let probe = Probe::workspace().expect("the workspace probe");
        let scratch = Scratch::new("obsidian-probe-install");
        probe.install(scratch.root()).expect("the install");

        let tree = probe.dir.join("vault");
        assert_eq!(
            files_under(&tree),
            files_under(&tree.join(probe.probe_root())),
            "the checked-in tree holds files outside the probe's root, which no install copies"
        );
        let named = named_paths(&probe);
        let unlisted: Vec<&String> = named
            .iter()
            .filter(|path| !listed_exactly(scratch.root(), path))
            .collect();
        assert!(
            unlisted.is_empty(),
            "installed under other bytes: {unlisted:?}"
        );
        assert!(
            named
                .iter()
                .any(|path| path.contains("pair-a/n422-re\u{301}sume\u{301}.md"))
                && named
                    .iter()
                    .any(|path| path.contains("pair-b/n422-r\u{e9}sum\u{e9}.md")),
            "the table keeps its decomposed and composed twins"
        );
    }

    /// **Every rename moves a composed checked-in name to a case path**, so
    /// the list holds no stray entry and git is handed no decomposed name.
    #[test]
    fn each_rename_moves_a_composed_checked_in_name_to_a_named_path() {
        let probe = Probe::workspace().expect("the workspace probe");
        let named = named_paths(&probe);
        assert!(!probe.renamed_on_install().is_empty());
        for rename in probe.renamed_on_install() {
            assert_ne!(rename.checked_in, rename.installed);
            assert!(
                named.contains(&rename.installed),
                "{rename:?} names no case path"
            );
            assert!(
                !rename.checked_in.contains('\u{301}'),
                "{rename:?} checks in a decomposed name"
            );
        }
    }

    /// **A failed install leaves no probe behind**, so the same vault takes
    /// the install again: a rename naming a file the tree does not hold fails
    /// after the copy, and the probe's root is gone afterwards.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: a probe whose install list names a missing file.
    fn a_failed_install_removes_its_partial_probe() {
        let scratch = Scratch::new("obsidian-probe-partial");
        let workspace = Probe::workspace().expect("the workspace probe");
        let broken = scratch.join("probe");
        copy_tree(&workspace.dir, &broken).expect("a copy of the probe");
        fs::write(
            broken.join("install.json"),
            r#"{"schema_version": 1, "renamed_on_install": [
                {"checked_in": "artifacts/scratch/norn-422/gone.md",
                 "installed": "artifacts/scratch/norn-422/still-gone.md"}]}"#,
        )
        .unwrap();
        let vault = scratch.join("vault");
        fs::create_dir(&vault).unwrap();

        let failed = Probe::load(&broken)
            .expect("the broken probe")
            .install(&vault)
            .expect_err("a rename of a missing file");
        assert!(failed.contains("the partial probe was removed"), "{failed}");
        assert!(fs::symlink_metadata(vault.join(workspace.probe_root())).is_err());
        workspace.install(&vault).expect("the install again");
    }

    /// **A probe lands fresh**: a vault already holding the probe's root is
    /// refused, and nothing in it is touched.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging an earlier run's probe.
    fn an_install_refuses_a_vault_that_already_holds_the_probe() {
        let probe = Probe::workspace().expect("the workspace probe");
        let scratch = Scratch::new("obsidian-probe-refusal");
        let earlier = scratch.join(probe.probe_root()).join("n422-results.md");
        fs::create_dir_all(earlier.parent().unwrap()).unwrap();
        fs::write(&earlier, b"an earlier run").unwrap();

        let refused = probe
            .install(scratch.root())
            .expect_err("an install over a probe");
        assert!(refused.contains("already holds a probe"), "{refused}");
        assert_eq!(fs::read(&earlier).unwrap(), b"an earlier run");
        assert!(
            !scratch
                .join(probe.probe_root())
                .join("n422-start.md")
                .exists()
        );
    }
}
