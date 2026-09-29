//! What the planner reads of a vault: a file's bytes, and whether a folder
//! stands and what it lists.

use norn_wire::{DocumentPath, FolderPath};

/// The vault as the planner reads it.
///
/// **Files, not the store.** A before-state is the hash of the bytes a target
/// is composed from, and the applier recomposes from what the file holds, so
/// both read the file: the store holds no document's exact bytes, and a hash
/// read from it could name bytes nobody composed against.
pub(crate) trait VaultView {
    /// Why the view could not answer: a machine failure, never absence.
    type Error;

    /// The bytes of the document at `path`, or `None` where no document
    /// stands there.
    fn file(&self, path: &DocumentPath) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Whether a folder stands at `folder`.
    fn folder_stands(&self, folder: &FolderPath) -> Result<bool, Self::Error>;

    /// The name of every entry directly inside `folder`, whatever its kind:
    /// a document, a folder, or anything else that keeps it from being
    /// empty.
    fn folder_entries(&self, folder: &FolderPath) -> Result<Vec<String>, Self::Error>;
}

#[cfg(test)]
pub(crate) mod memory {
    //! A vault held in memory, for the planner's own tests.

    use std::collections::{BTreeMap, BTreeSet};
    use std::convert::Infallible;

    use norn_wire::{DocumentPath, FolderPath};

    use super::VaultView;

    /// Files by path, and folders standing empty. A folder stands where it
    /// was made empty or where anything stands below it.
    #[derive(Default)]
    pub(crate) struct MemoryVault {
        files: BTreeMap<String, Vec<u8>>,
        empty_folders: BTreeSet<String>,
    }

    impl MemoryVault {
        pub(crate) fn with(files: &[(&str, &str)]) -> Self {
            let mut vault = MemoryVault::default();
            for (path, content) in files {
                vault
                    .files
                    .insert((*path).to_string(), content.as_bytes().to_vec());
            }
            vault
        }

        pub(crate) fn with_empty_folder(mut self, folder: &str) -> Self {
            self.empty_folders.insert(folder.to_string());
            self
        }

        fn every_name(&self) -> impl Iterator<Item = &String> {
            self.files.keys().chain(self.empty_folders.iter())
        }
    }

    impl VaultView for MemoryVault {
        type Error = Infallible;

        fn file(&self, path: &DocumentPath) -> Result<Option<Vec<u8>>, Infallible> {
            Ok(self.files.get(path.as_str()).cloned())
        }

        fn folder_stands(&self, folder: &FolderPath) -> Result<bool, Infallible> {
            let prefix = format!("{}/", folder.as_str());
            Ok(self.empty_folders.contains(folder.as_str())
                || self.every_name().any(|name| name.starts_with(&prefix)))
        }

        fn folder_entries(&self, folder: &FolderPath) -> Result<Vec<String>, Infallible> {
            let prefix = format!("{}/", folder.as_str());
            let names: BTreeSet<String> = self
                .every_name()
                .filter_map(|name| name.strip_prefix(&prefix))
                .map(|rest| rest.split('/').next().unwrap_or(rest).to_string())
                .collect();
            Ok(names.into_iter().collect())
        }
    }
}
