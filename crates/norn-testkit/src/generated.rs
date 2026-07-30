//! Generated vault documents, in memory.
//!
//! A suite that needs realistic documents rather than hand-authored ones asks
//! here: [`documents`] generates a tree with `norn-fixtures`, reads its
//! Markdown back, removes the tree, and hands over the bytes. The scaffolding
//! lives here for the reason all of it does — the filesystem effect belongs to
//! one place, and a suite that only wants documents should not have to hold a
//! temporary directory to get them.

use std::path::{Path, PathBuf};

use norn_fixtures::profile::Profile;

/// One generated Markdown document.
///
/// The path it was generated at is kept only to order the batch: the tree is
/// removed before it is handed over, so a path a caller could read would name
/// nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedDocument {
    pub(crate) path: String,
    pub text: String,
}

/// Every Markdown document of the tree `(profile, seed)` generates.
///
/// The tree is written to a temporary directory and removed before this
/// returns, so nothing outlives the call and no caller learns a path.
pub fn documents(profile: &str, seed: u64) -> Result<Vec<GeneratedDocument>, String> {
    let profile = Profile::by_name(profile)
        .ok_or_else(|| format!("`{profile}` is not a profile norn-fixtures knows"))?;
    let root = scratch_root(&format!("{}-{seed}", profile.name));
    let outcome = generate_and_read(&profile, seed, &root);
    remove(&root);
    outcome
}

fn generate_and_read(
    profile: &Profile,
    seed: u64,
    root: &Path,
) -> Result<Vec<GeneratedDocument>, String> {
    norn_fixtures::generate(profile, seed, root)
        .map_err(|error| format!("could not generate `{}`: {error}", profile.name))?;
    let mut documents = Vec::new();
    read_markdown(root, "", &mut documents)?;
    documents.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(documents)
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: reads the temporary tree this module just generated.
fn read_markdown(
    directory: &Path,
    prefix: &str,
    documents: &mut Vec<GeneratedDocument>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(directory)
        .map_err(|error| format!("could not read {}: {error}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("could not read a directory entry: {error}"))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| format!("{} holds a non-UTF-8 name", directory.display()))?;
        let path = entry.path();
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if path.is_dir() {
            read_markdown(&path, &relative, documents)?;
        } else if name.ends_with(".md") {
            let text = std::fs::read_to_string(&path)
                .map_err(|error| format!("could not read {}: {error}", path.display()))?;
            documents.push(GeneratedDocument {
                path: relative,
                text,
            });
        }
    }
    Ok(())
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: the scratch tree is this module's to place.
fn scratch_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "norn-generated-{label}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ))
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: the scratch tree is this module's to remove.
fn remove(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
}
