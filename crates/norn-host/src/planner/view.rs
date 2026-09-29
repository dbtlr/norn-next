//! What the planner reads of a vault: what stands at a name, and whether a
//! folder stands and what it lists — every name read under the identity rule
//! the vault's root proved.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use norn_fs::{NormalizedPath, PathKind, PathNormalizer, Reach, Refusal, SkipReason, WalkError};
use norn_wire::{ContentHash, DocumentPath};

/// The vault as the planner reads it.
///
/// **Files, not the store.** A before-state is the hash of the bytes a target
/// is composed from, and the applier recomposes from what the file holds, so
/// both read the file: the store holds no document's exact bytes, and a hash
/// read from it could name bytes nobody composed against.
///
/// **One identity rule.** Every name the planner compares is produced by
/// [`normalizer`](Self::normalizer) — `norn-fs`'s one path-spelling
/// normalization point, under the case behavior the root proved — so two
/// spellings of one file are one identity, and the planner derives no spelling
/// rule of its own.
pub(crate) trait VaultView {
    /// Why the view could not answer: a machine failure, never absence.
    type Error;

    /// The root's identity rule.
    fn normalizer(&self) -> &PathNormalizer;

    /// What stands at `path`.
    fn entry(&self, path: &NormalizedPath) -> Result<Entry, Self::Error>;

    /// Whether a folder stands at `folder`.
    fn folder_stands(&self, folder: &NormalizedPath) -> Result<bool, Self::Error>;

    /// The name of every entry directly inside `folder`, whatever its kind:
    /// a document, a folder, or anything else that keeps it from being
    /// empty. Nothing where no folder stands.
    fn folder_names(&self, folder: &NormalizedPath) -> Result<Vec<OsString>, Self::Error>;
}

/// What stands at one name.
#[derive(Clone, Debug)]
pub(crate) enum Entry {
    /// Nothing. `at` is the spelling a document made here takes: each folder
    /// above it that stands, as its parent lists it, and the rest as asked.
    Absent { at: DocumentPath },
    /// A document, at the spelling the tree lists, with its bytes and their
    /// hash from one read.
    Document {
        at: DocumentPath,
        bytes: Arc<[u8]>,
        hash: ContentHash,
    },
    /// A folder.
    Folder,
    /// Something no document can be read from or made at: an entry that is
    /// neither a document nor a folder, a name beneath one that is not a
    /// folder, or a place the vault's walk does not enter.
    Blocked { detail: String },
}

/// The wire's content hash of `hash`: the filesystem layer's SHA-256, spelled
/// with its algorithm.
pub(crate) fn wire_hash(hash: norn_fs::ContentHash) -> ContentHash {
    ContentHash::new(format!("sha256:{}", hash.to_hex()))
        .expect("a SHA-256 digest spells a content hash")
}

/// `path` as a document path: every normalized spelling of a document path
/// parses as one, since normalizing only drops names.
pub(crate) fn document_path(path: &Path) -> Option<DocumentPath> {
    DocumentPath::new(path.to_str()?).ok()
}

/// Why the store's index cannot hold a document at `spelling`, or `None`
/// where it can.
///
/// The wire admits any relative, non-empty path; the index keys a document by
/// the store's own grammar, which also refuses a backslash, a control byte, an
/// empty or `.`/`..` segment, and a leaf whose stem is `.` or `..`. A plan
/// whose target the index cannot hold would publish a file the vault's
/// derivation then quarantines, so the planner asks the store's rule rather
/// than restating it. `spelling` is the normalized one, as the store keys by.
pub(crate) fn unholdable(spelling: &DocumentPath) -> Option<String> {
    norn_store::DocumentPath::new(spelling.as_str())
        .err()
        .map(|refusal| refusal.to_string())
}

/// A view that reads each name once and answers every later read of it with
/// what the first read found.
///
/// Planning orders its operations, composes them, and composes again after
/// leaving an operation out, and one planning is one observation of each
/// file: every pass then composes from the same before-states, and a file is
/// read once however many passes it takes. A name is remembered by its
/// identity, so two spellings of one file are one read.
pub(crate) struct Remembered<'view, V> {
    view: &'view V,
    entries: RefCell<BTreeMap<NormalizedPath, Entry>>,
}

impl<'view, V> Remembered<'view, V> {
    pub(crate) fn over(view: &'view V) -> Self {
        Remembered {
            view,
            entries: RefCell::new(BTreeMap::new()),
        }
    }
}

impl<V: VaultView> VaultView for Remembered<'_, V> {
    type Error = V::Error;

    fn normalizer(&self) -> &PathNormalizer {
        self.view.normalizer()
    }

    fn entry(&self, path: &NormalizedPath) -> Result<Entry, V::Error> {
        if let Some(read) = self.entries.borrow().get(path) {
            return Ok(read.clone());
        }
        let read = self.view.entry(path)?;
        self.entries.borrow_mut().insert(path.clone(), read.clone());
        Ok(read)
    }

    fn folder_stands(&self, folder: &NormalizedPath) -> Result<bool, V::Error> {
        self.view.folder_stands(folder)
    }

    fn folder_names(&self, folder: &NormalizedPath) -> Result<Vec<OsString>, V::Error> {
        self.view.folder_names(folder)
    }
}

/// The vault on disk, read through `norn-fs`'s anchored descent: no link is
/// followed, a place the walk does not enter holds nothing readable, and on a
/// root that folds case a name stands only at the spelling its folder lists.
pub(crate) struct TreeView {
    root: PathBuf,
    vault: norn_fs::Vault,
}

/// Why the vault on disk could not be read.
#[derive(Debug)]
pub(crate) enum TreeViewError {
    /// The descent to a name failed.
    Walk(WalkError),
    /// A document's read failed.
    Read(Refusal),
}

impl std::fmt::Display for TreeViewError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TreeViewError::Walk(error) => error.fmt(formatter),
            TreeViewError::Read(refusal) => refusal.fmt(formatter),
        }
    }
}

impl std::error::Error for TreeViewError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TreeViewError::Walk(error) => Some(error),
            TreeViewError::Read(refusal) => Some(refusal),
        }
    }
}

impl TreeView {
    /// The vault at `root`, whose walk does not enter `exclusions`.
    pub(crate) fn open(root: &Path, exclusions: &[PathBuf]) -> Result<Self, TreeViewError> {
        let vault = norn_fs::Vault::open(root, exclusions).map_err(TreeViewError::Walk)?;
        Ok(TreeView {
            root: root.to_owned(),
            vault,
        })
    }

    fn reach(&self, path: &Path) -> Result<Reach, TreeViewError> {
        self.vault.reach(path).map_err(TreeViewError::Walk)
    }

    /// The spelling a document made at `path` takes: the deepest folder above
    /// it that stands, as the tree lists it, and the rest as asked.
    fn spelled_where_absent(&self, path: &NormalizedPath) -> Result<Entry, TreeViewError> {
        let asked = path.as_path();
        for above in asked.ancestors().skip(1) {
            if above.as_os_str().is_empty() {
                break;
            }
            if let Reach::Stands {
                kind: PathKind::Directory,
                at,
            } = self.reach(above)?
            {
                let rest = asked
                    .strip_prefix(above)
                    .expect("an ancestor prefixes its path");
                return Ok(spelled(&at.as_path().join(rest)));
            }
        }
        Ok(spelled(asked))
    }
}

/// `Absent` at `at`, or `Blocked` where it is no document path.
fn spelled(at: &Path) -> Entry {
    match document_path(at) {
        Some(at) => Entry::Absent { at },
        None => not_a_document_path(at),
    }
}

fn not_a_document_path(at: &Path) -> Entry {
    Entry::Blocked {
        detail: format!("`{}` is not spelled as a document path", at.display()),
    }
}

impl VaultView for TreeView {
    type Error = TreeViewError;

    fn normalizer(&self) -> &PathNormalizer {
        self.vault.normalizer()
    }

    fn entry(&self, path: &NormalizedPath) -> Result<Entry, TreeViewError> {
        let (kind, at) = match self.reach(path.as_path())? {
            Reach::Refused(skip) => {
                let root = skip.path().as_path().display();
                let detail = match skip.reason() {
                    SkipReason::UnderAnEntry => format!(
                        "`{}` lies beneath an entry that is not a folder",
                        path.as_path().display()
                    ),
                    SkipReason::SymbolicLink(_) => {
                        format!("`{root}` is a symbolic link, which the vault does not follow")
                    }
                    SkipReason::HostExclusion
                    | SkipReason::Mechanism
                    | SkipReason::Shadow
                    | SkipReason::SpecialFile(_)
                    | SkipReason::Vanished => {
                        format!("`{root}` is a place the vault does not read documents at")
                    }
                };
                return Ok(Entry::Blocked { detail });
            }
            Reach::Stands { kind, at } => (kind, at),
        };
        Ok(match kind {
            PathKind::Missing => self.spelled_where_absent(path)?,
            PathKind::Directory => Entry::Folder,
            PathKind::Other => Entry::Blocked {
                detail: format!(
                    "something that is neither a document nor a folder stands at `{}`",
                    at.as_path().display()
                ),
            },
            PathKind::RegularFile => {
                let Some(spelling) = document_path(at.as_path()) else {
                    return Ok(not_a_document_path(at.as_path()));
                };
                match norn_fs::read_optional_and_hash(&self.root, at.as_path())
                    .map_err(TreeViewError::Read)?
                {
                    // Taken away between the descent and the read: nothing
                    // stands there now.
                    None => self.spelled_where_absent(path)?,
                    Some(read) => {
                        let (bytes, hash) = read.into_parts();
                        Entry::Document {
                            at: spelling,
                            bytes: Arc::from(bytes),
                            hash: wire_hash(hash),
                        }
                    }
                }
            }
        })
    }

    fn folder_stands(&self, folder: &NormalizedPath) -> Result<bool, TreeViewError> {
        Ok(matches!(
            self.reach(folder.as_path())?,
            Reach::Stands {
                kind: PathKind::Directory,
                ..
            }
        ))
    }

    fn folder_names(&self, folder: &NormalizedPath) -> Result<Vec<OsString>, TreeViewError> {
        Ok(self
            .vault
            .folder_names(folder.as_path())
            .map_err(TreeViewError::Walk)?
            .unwrap_or_default())
    }
}

#[cfg(test)]
pub(crate) mod memory {
    //! A vault held in memory, for the planner's own tests, under either case
    //! behavior a root can prove.

    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::convert::Infallible;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use norn_fs::{CaseSensitivity, NormalizedPath, PathNormalizer};

    use super::{Entry, VaultView, document_path, wire_hash};

    /// Files by spelling, and folders standing empty. A folder stands where it
    /// was made empty or where anything stands below it. Names are compared
    /// under the vault's case behavior: exactly, or with ASCII case folded.
    pub(crate) struct MemoryVault {
        normalizer: PathNormalizer,
        files: BTreeMap<String, Arc<[u8]>>,
        empty_folders: BTreeSet<String>,
        /// How many times each name was read.
        pub(crate) reads: RefCell<BTreeMap<String, usize>>,
    }

    impl Default for MemoryVault {
        fn default() -> Self {
            MemoryVault {
                normalizer: PathNormalizer::for_sensitivity(CaseSensitivity::Sensitive),
                files: BTreeMap::new(),
                empty_folders: BTreeSet::new(),
                reads: RefCell::default(),
            }
        }
    }

    impl MemoryVault {
        pub(crate) fn with(files: &[(&str, &str)]) -> Self {
            let mut vault = MemoryVault::default();
            for (path, content) in files {
                vault
                    .files
                    .insert((*path).to_string(), Arc::from(content.as_bytes()));
            }
            vault
        }

        /// This vault on a root that folds ASCII case.
        pub(crate) fn folding_case(mut self) -> Self {
            self.normalizer = PathNormalizer::for_sensitivity(CaseSensitivity::Insensitive);
            self
        }

        pub(crate) fn with_empty_folder(mut self, folder: &str) -> Self {
            self.empty_folders.insert(folder.to_string());
            self
        }

        fn identity(&self, spelling: &str) -> NormalizedPath {
            self.normalizer
                .normalize(Path::new(spelling))
                .expect("a stored spelling is a vault path")
        }

        /// Every name that stands, as its own spelling.
        fn every_name(&self) -> impl Iterator<Item = &String> {
            self.files.keys().chain(self.empty_folders.iter())
        }

        /// The spelling of the folder `folder` names, where one stands: the
        /// leading run of a name below it.
        fn folder_spelling(&self, folder: &NormalizedPath) -> Option<PathBuf> {
            let depth = folder.as_path().components().count();
            let made_empty = self
                .empty_folders
                .iter()
                .find(|name| self.identity(name) == *folder)
                .map(PathBuf::from);
            made_empty.or_else(|| {
                self.every_name().find_map(|name| {
                    let run: PathBuf = Path::new(name).components().take(depth).collect();
                    let below = Path::new(name).components().count() > depth;
                    (below && self.identity(&run.to_string_lossy()) == *folder).then_some(run)
                })
            })
        }
    }

    impl VaultView for MemoryVault {
        type Error = Infallible;

        fn normalizer(&self) -> &PathNormalizer {
            &self.normalizer
        }

        fn entry(&self, path: &NormalizedPath) -> Result<Entry, Infallible> {
            *self
                .reads
                .borrow_mut()
                .entry(path.as_path().to_string_lossy().into_owned())
                .or_default() += 1;
            let asked = path.as_path();
            for above in asked.ancestors().skip(1) {
                if above.as_os_str().is_empty() {
                    break;
                }
                let above = self.identity(&above.to_string_lossy());
                if self.files.keys().any(|name| self.identity(name) == above) {
                    return Ok(Entry::Blocked {
                        detail: format!(
                            "`{}` lies beneath an entry that is not a folder",
                            asked.display()
                        ),
                    });
                }
            }
            if let Some((name, bytes)) = self
                .files
                .iter()
                .find(|(name, _)| self.identity(name) == *path)
            {
                return Ok(Entry::Document {
                    at: document_path(Path::new(name)).expect("a stored document path"),
                    bytes: bytes.clone(),
                    hash: wire_hash(norn_fs::ContentHash::of(bytes)),
                });
            }
            if self.folder_stands(path)? {
                return Ok(Entry::Folder);
            }
            for above in asked.ancestors().skip(1) {
                if above.as_os_str().is_empty() {
                    break;
                }
                if let Some(spelled) =
                    self.folder_spelling(&self.identity(&above.to_string_lossy()))
                {
                    let rest = asked.strip_prefix(above).expect("an ancestor prefixes it");
                    let at = spelled.join(rest);
                    return Ok(Entry::Absent {
                        at: document_path(&at).expect("a document path"),
                    });
                }
            }
            Ok(Entry::Absent {
                at: document_path(asked).expect("a document path"),
            })
        }

        fn folder_stands(&self, folder: &NormalizedPath) -> Result<bool, Infallible> {
            Ok(self.folder_spelling(folder).is_some())
        }

        fn folder_names(&self, folder: &NormalizedPath) -> Result<Vec<OsString>, Infallible> {
            let depth = folder.as_path().components().count();
            let names: BTreeSet<OsString> = self
                .every_name()
                .filter_map(|name| {
                    let mut components = Path::new(name).components();
                    let run: PathBuf = components.by_ref().take(depth).collect();
                    let next = components.next()?;
                    (self.identity(&run.to_string_lossy()) == *folder)
                        .then(|| next.as_os_str().to_owned())
                })
                .collect();
            Ok(names.into_iter().collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use norn_testkit::scratch::Scratch;

    use super::*;

    /// A tree on disk with a document, a folder, a link and a name beneath
    /// a document.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the tree a case reads.
    fn tree() -> (Scratch, TreeView) {
        let scratch = Scratch::new("planner-view");
        std::fs::create_dir_all(scratch.join("folder")).expect("a folder");
        std::fs::write(scratch.join("folder/a.md"), "A").expect("a document");
        std::os::unix::fs::symlink("folder", scratch.join("link")).expect("a link");
        let view = TreeView::open(scratch.root(), &[]).expect("a vault");
        (scratch, view)
    }

    fn entry(view: &TreeView, at: &str) -> Entry {
        let identity = view
            .normalizer()
            .normalize(Path::new(at))
            .expect("a vault path");
        view.entry(&identity).expect("a readable tree")
    }

    #[test]
    fn the_tree_view_reads_a_document_with_the_hash_of_its_bytes() {
        let (_scratch, view) = tree();
        let Entry::Document { at, bytes, hash } = entry(&view, "folder/a.md") else {
            panic!("a document stands at folder/a.md");
        };
        assert_eq!(at.as_str(), "folder/a.md");
        assert_eq!(&*bytes, b"A");
        assert_eq!(hash, wire_hash(norn_fs::ContentHash::of(b"A")));
    }

    #[test]
    fn the_tree_view_tells_a_folder_an_absence_and_a_blocked_name_apart() {
        let (_scratch, view) = tree();
        assert!(matches!(entry(&view, "folder"), Entry::Folder));
        assert!(
            matches!(entry(&view, "folder/new.md"), Entry::Absent { at } if at.as_str() == "folder/new.md")
        );
        let Entry::Blocked { detail } = entry(&view, "folder/a.md/under.md") else {
            panic!("nothing is made beneath a document");
        };
        assert!(detail.contains("beneath"), "{detail}");
        let Entry::Blocked { detail } = entry(&view, "link/a.md") else {
            panic!("nothing is read through a link");
        };
        assert!(detail.contains("symbolic link"), "{detail}");
    }

    #[test]
    fn the_tree_view_lists_a_folder_s_names() {
        let (_scratch, view) = tree();
        let folder = view
            .normalizer()
            .normalize(Path::new("folder"))
            .expect("a vault path");
        assert!(view.folder_stands(&folder).expect("a readable tree"));
        assert_eq!(
            view.folder_names(&folder).expect("a readable tree"),
            vec![OsString::from("a.md")]
        );
    }

    /// On a volume that folds case, a spelling the fold equates is the
    /// document at the spelling the tree lists. Binds only where the volume
    /// folds.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the tree a case reads.
    fn on_a_folding_tree_another_spelling_reads_the_listed_document() {
        let scratch = Scratch::new("planner-view-folding");
        std::fs::write(scratch.join("Note.md"), "N").expect("a document");
        let view = TreeView::open(scratch.root(), &[]).expect("a vault");
        if view.normalizer().case_sensitivity() != norn_fs::CaseSensitivity::Insensitive {
            return;
        }
        let Entry::Document { at, .. } = entry(&view, "note.md") else {
            panic!("the document stands at its listed spelling");
        };
        assert_eq!(at.as_str(), "Note.md");
    }
}
