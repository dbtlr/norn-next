//! What the planner reads of a vault.

use norn_wire::DocumentPath;

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
}

#[cfg(test)]
pub(crate) mod memory {
    //! A vault held in memory, for the planner's own tests.

    use std::collections::BTreeMap;
    use std::convert::Infallible;

    use norn_wire::DocumentPath;

    use super::VaultView;

    /// Files by path.
    #[derive(Default)]
    pub(crate) struct MemoryVault {
        files: BTreeMap<String, Vec<u8>>,
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
    }

    impl VaultView for MemoryVault {
        type Error = Infallible;

        fn file(&self, path: &DocumentPath) -> Result<Option<Vec<u8>>, Infallible> {
            Ok(self.files.get(path.as_str()).cloned())
        }
    }
}
