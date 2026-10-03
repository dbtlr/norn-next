//! What the planner reads of a vault: what stands at a name, and whether a
//! folder stands and what it lists — every name read under the identity rule
//! the vault's root proved.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use norn_fs::{NormalizedPath, PathKind, PathNormalizer, Reach, Refusal, SkipReason, WalkError};
use norn_wire::{ContentHash, DocumentPath, FilePath};

use super::control::SchemaPlace;

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

    /// What stands at `path`, the in-vault path a control file lives at
    /// ([`super::control`]), read as that control file rather than as a
    /// document: a [`Entry::Document`] holding its bytes where it stands,
    /// [`Entry::Absent`] at `path` where nothing does, and
    /// [`Entry::Blocked`] where something that is no file stands there.
    ///
    /// **Its own read, because a control file is no document.** The vault's
    /// walk does not enter the schema's path, so [`entry`](Self::entry)
    /// answers it as a place no document can be at; the host reads both
    /// control files directly beneath the root, and so does this.
    fn control_entry(&self, path: &NormalizedPath) -> Result<Entry, Self::Error>;

    /// Whether a folder stands at `folder`.
    fn folder_stands(&self, folder: &NormalizedPath) -> Result<bool, Self::Error>;

    /// Hand `visit` the name of every entry directly inside `folder`,
    /// whatever its kind — a document, a folder, or anything else that keeps
    /// it from being empty — in no promised order, until it breaks. Nothing
    /// where no folder stands.
    ///
    /// **The listing streams**: no name is kept once visited, so a listing
    /// holds one name however wide the folder is.
    fn visit_folder_names(
        &self,
        folder: &NormalizedPath,
        visit: &mut dyn FnMut(&OsStr) -> ControlFlow<()>,
    ) -> Result<(), Self::Error>;

    /// Hand `visit` the name of every entry directly inside the vault root,
    /// as [`visit_folder_names`](Self::visit_folder_names) lists a folder's.
    ///
    /// The root is its own method because a [`NormalizedPath`] cannot be
    /// empty, so no `folder` names it; `norn-fs` spells the same listing
    /// `visit_folder_names("")`.
    fn visit_root_names(
        &self,
        visit: &mut dyn FnMut(&OsStr) -> ControlFlow<()>,
    ) -> Result<(), Self::Error>;

    /// Everything beneath `folder`, at any depth, as a folder move takes it
    /// ([`FolderContents`]); `None` where no folder stands there.
    fn folder_contents(
        &self,
        folder: &NormalizedPath,
    ) -> Result<Option<FolderContents>, Self::Error>;
}

/// What a folder holds, at any depth, as a folder move takes it: the
/// documents it moves, and what it leaves behind.
///
/// **A document is what the vault derives one from**: a file whose extension
/// is the document extension, in any ASCII case, at a path the store's index
/// can hold. Every other file — an attachment, a file the vault does not
/// read, a document file at a path the index refuses — and every place
/// beneath the folder the vault's walk does not enter — a link, a special
/// file, an excluded root — is left behind, named at the spelling the tree
/// lists it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct FolderContents {
    /// Every document beneath the folder, at the spelling the tree lists, in
    /// path order.
    pub(crate) documents: Vec<DocumentPath>,
    /// Every other file and every place not entered beneath it, in path
    /// order, each at the spelling the tree lists it — a name that is not
    /// UTF-8 at its lossy spelling, its undecodable bytes replaced.
    pub(crate) left: Vec<FilePath>,
}

impl FolderContents {
    /// Take the file at `path`: a document where it is one, left behind
    /// otherwise.
    fn take(&mut self, path: &Path) {
        let spelled = path.to_string_lossy();
        match document_path(path).filter(|_| crate::production::is_markdown(path)) {
            Some(document) => self.documents.push(document),
            None => self.leave(&spelled),
        }
    }

    /// Leave behind what stands at `spelled`. A file path refuses only an
    /// empty spelling and one starting at a filesystem root, and a path the
    /// walk lists beneath a folder is neither, lossy spellings included, so
    /// nothing left behind goes unnamed; the refusal arm is unreachable.
    fn leave(&mut self, spelled: &str) {
        if let Ok(left) = FilePath::new(spelled) {
            self.left.push(left);
        }
    }
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
    Blocked { detail: String, barrier: Barrier },
}

/// What keeps a document from a name.
///
/// **The two differ in who can lift them.** Another writer can take away an
/// entry that stands in the way, so a plan resolved before it arrived meets a
/// taken name, which the applier judges as it would any; no writer makes a
/// place the vault does not read into one it does, so a plan naming one is
/// not what its operations do, whatever it says of the name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Barrier {
    /// Something that is not a folder stands at the name or above it: a
    /// document, a link, or any other entry.
    Occupied,
    /// The vault reads no documents there: its mechanism folder, the shadow
    /// home, a root the host excludes, or a spelling that is no document path.
    Closed,
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
    /// Each control file read, apart from [`Self::entries`]: one name read
    /// as a document and as a control file is two readings.
    controls: RefCell<BTreeMap<NormalizedPath, Entry>>,
}

impl<'view, V> Remembered<'view, V> {
    pub(crate) fn over(view: &'view V) -> Self {
        Remembered {
            view,
            entries: RefCell::new(BTreeMap::new()),
            controls: RefCell::new(BTreeMap::new()),
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

    fn control_entry(&self, path: &NormalizedPath) -> Result<Entry, V::Error> {
        if let Some(read) = self.controls.borrow().get(path) {
            return Ok(read.clone());
        }
        let read = self.view.control_entry(path)?;
        self.controls
            .borrow_mut()
            .insert(path.clone(), read.clone());
        Ok(read)
    }

    fn folder_stands(&self, folder: &NormalizedPath) -> Result<bool, V::Error> {
        self.view.folder_stands(folder)
    }

    fn visit_folder_names(
        &self,
        folder: &NormalizedPath,
        visit: &mut dyn FnMut(&OsStr) -> ControlFlow<()>,
    ) -> Result<(), V::Error> {
        self.view.visit_folder_names(folder, visit)
    }

    fn visit_root_names(
        &self,
        visit: &mut dyn FnMut(&OsStr) -> ControlFlow<()>,
    ) -> Result<(), V::Error> {
        self.view.visit_root_names(visit)
    }

    fn folder_contents(&self, folder: &NormalizedPath) -> Result<Option<FolderContents>, V::Error> {
        self.view.folder_contents(folder)
    }
}

/// The vault on disk, read through `norn-fs`'s anchored descent: no link is
/// followed, a place the walk does not enter holds nothing readable, and on a
/// root that folds case a name stands only at the spelling its folder lists.
pub(crate) struct TreeView {
    root: PathBuf,
    exclusions: Vec<PathBuf>,
    vault: norn_fs::Vault,
    schema: SchemaAt,
}

/// Where the view reads the vault schema's role, resolved when it opens
/// ([`SchemaPlace`]).
enum SchemaAt {
    /// Beneath the root, at this path relative to it.
    InVault(PathBuf),
    /// Outside the root: the folder, and the file's name in it.
    Outside { folder: PathBuf, name: PathBuf },
    /// A place no schema write lands at, and why, in words.
    Nowhere(String),
}

impl SchemaAt {
    /// Where `place` is read, and whether a write can land there: a place
    /// outside the root only where the vault's shadow home publishes into
    /// its folder ([`norn_fs::ShadowHome::publishes_outside`]).
    fn of(place: &SchemaPlace) -> Self {
        match place {
            SchemaPlace::InVault(relative) => SchemaAt::InVault(relative.clone()),
            SchemaPlace::Outside {
                folder,
                name,
                shadows,
            } => match shadows.publishes_outside(folder) {
                Ok(()) => SchemaAt::Outside {
                    folder: folder.clone(),
                    name: name.clone(),
                },
                Err(unpublishable) => SchemaAt::Nowhere(format!(
                    "the schema source `{}` cannot be written: {unpublishable}",
                    folder.join(name).display()
                )),
            },
            SchemaPlace::NoFile(source) => SchemaAt::Nowhere(format!(
                "the schema source `{}` names no file",
                source.display()
            )),
        }
    }
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
    /// The vault at `root`, whose walk does not enter `exclusions`, reading
    /// its schema at `schema`.
    pub(crate) fn open(
        root: &Path,
        exclusions: &[PathBuf],
        schema: &SchemaPlace,
    ) -> Result<Self, TreeViewError> {
        let vault = norn_fs::Vault::open(root, exclusions).map_err(TreeViewError::Walk)?;
        Ok(TreeView {
            root: root.to_owned(),
            exclusions: exclusions.to_vec(),
            vault,
            schema: SchemaAt::of(schema),
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
        barrier: Barrier::Closed,
    }
}

impl TreeView {
    /// `relative` beneath `anchor` as a reader names it: relative to the
    /// root where it lies beneath it, and whole otherwise.
    fn spelled_for_humans(&self, anchor: &Path, relative: &Path) -> String {
        if anchor == self.root {
            relative.display().to_string()
        } else {
            anchor.join(relative).display().to_string()
        }
    }

    /// Whether `path` is the default schema's path, `.norn/schema.yaml`,
    /// under the root's identity rule.
    fn is_the_default_schema(&self, path: &NormalizedPath) -> bool {
        let schema = super::control::control_path(norn_wire::ControlFile::Schema);
        self.normalizer()
            .normalize(Path::new(schema.as_str()))
            .is_ok_and(|schema| schema == *path)
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
                let (detail, barrier) = match skip.reason() {
                    SkipReason::UnderAnEntry => (
                        format!(
                            "`{}` lies beneath an entry that is not a folder",
                            path.as_path().display()
                        ),
                        Barrier::Occupied,
                    ),
                    SkipReason::SymbolicLink(_) => (
                        format!("`{root}` is a symbolic link, which the vault does not follow"),
                        Barrier::Occupied,
                    ),
                    SkipReason::SpecialFile(_) | SkipReason::Vanished => (
                        format!("`{root}` is a place the vault does not read documents at"),
                        Barrier::Occupied,
                    ),
                    SkipReason::HostExclusion | SkipReason::Mechanism | SkipReason::Shadow => (
                        format!("`{root}` is a place the vault does not read documents at"),
                        Barrier::Closed,
                    ),
                };
                return Ok(Entry::Blocked { detail, barrier });
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
                barrier: Barrier::Occupied,
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

    /// **Read as the host reads a control file**: `norn-fs`'s contained read
    /// of an optional control file, which follows no link, answers a missing
    /// name as absence and refuses anything at the name that is not a regular
    /// file. That refusal — a link, a folder, a name the account cannot open
    /// — is answered as a place no control file can be written, in its own
    /// words, so a plan writing there does not resolve rather than failing
    /// the read of the vault.
    ///
    /// **The schema is read where the registration reads it** ([ADR
    /// 0034]): the schema's role, named at the default schema's path, is
    /// read at the [`SchemaPlace`] the view opened over — that default, a
    /// `schema_source` inside the vault, or one outside it — and answered at
    /// the role's path. A place no write can land at — a source naming no
    /// file, or one outside the vault the shadow home cannot publish into —
    /// is a place no control file is written, in its own words.
    ///
    /// [ADR 0034]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0034-a-schema-write-lands-where-the-registration-reads-the-schema.md
    fn control_entry(&self, path: &NormalizedPath) -> Result<Entry, TreeViewError> {
        let Some(at) = document_path(path.as_path()) else {
            return Ok(not_a_document_path(path.as_path()));
        };
        let (anchor, relative) = if self.is_the_default_schema(path) {
            match &self.schema {
                SchemaAt::InVault(relative) => (self.root.as_path(), relative.as_path()),
                SchemaAt::Outside { folder, name } => (folder.as_path(), name.as_path()),
                SchemaAt::Nowhere(detail) => {
                    return Ok(Entry::Blocked {
                        detail: detail.clone(),
                        barrier: Barrier::Closed,
                    });
                }
            }
        } else {
            (self.root.as_path(), path.as_path())
        };
        Ok(match norn_fs::read_if_present_and_hash(anchor, relative) {
            Ok(Some(read)) => {
                let (bytes, hash) = read.into_parts();
                Entry::Document {
                    at,
                    bytes: Arc::from(bytes),
                    hash: wire_hash(hash),
                }
            }
            Ok(None) => Entry::Absent { at },
            Err(refusal) => Entry::Blocked {
                detail: format!(
                    "`{}` cannot hold a control file: {refusal}",
                    self.spelled_for_humans(anchor, relative)
                ),
                barrier: Barrier::Occupied,
            },
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

    fn visit_folder_names(
        &self,
        folder: &NormalizedPath,
        visit: &mut dyn FnMut(&OsStr) -> ControlFlow<()>,
    ) -> Result<(), TreeViewError> {
        self.vault
            .visit_folder_names(folder.as_path(), visit)
            .map(drop)
            .map_err(TreeViewError::Walk)
    }

    fn visit_root_names(
        &self,
        visit: &mut dyn FnMut(&OsStr) -> ControlFlow<()>,
    ) -> Result<(), TreeViewError> {
        self.vault
            .visit_folder_names(Path::new(""), visit)
            .map(drop)
            .map_err(TreeViewError::Walk)
    }

    /// **The vault's own walk, narrowed to the folder**: it follows no link,
    /// enters no place the vault's walk does not, and yields what it finds
    /// in path order at the spelling the tree lists, each place it does not
    /// enter stated as a skip, which is left behind.
    fn folder_contents(
        &self,
        folder: &NormalizedPath,
    ) -> Result<Option<FolderContents>, TreeViewError> {
        if !self.folder_stands(folder)? {
            return Ok(None);
        }
        let walk = norn_fs::walk_subtree(&self.root, folder.as_path(), &self.exclusions)
            .map_err(TreeViewError::Walk)?;
        let mut contents = FolderContents::default();
        for fact in walk {
            match fact.map_err(TreeViewError::Walk)? {
                norn_fs::WalkFact::File(file) => contents.take(file.path().as_path()),
                norn_fs::WalkFact::Skipped(skip) => {
                    contents.leave(&skip.path().as_path().to_string_lossy());
                }
            }
        }
        Ok(Some(contents))
    }
}

#[cfg(test)]
pub(crate) mod memory {
    //! A vault held in memory, for the planner's own tests, under either case
    //! behavior a root can prove.

    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::convert::Infallible;
    use std::ffi::{OsStr, OsString};
    use std::ops::ControlFlow;
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
        /// How many times each folder was listed, the root as the empty
        /// name.
        pub(crate) listings: RefCell<BTreeMap<String, usize>>,
    }

    impl Default for MemoryVault {
        fn default() -> Self {
            MemoryVault {
                normalizer: PathNormalizer::for_sensitivity(CaseSensitivity::Sensitive),
                files: BTreeMap::new(),
                empty_folders: BTreeSet::new(),
                reads: RefCell::default(),
                listings: RefCell::default(),
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

        /// A vault holding `files` as raw bytes, which need not be text.
        pub(crate) fn with_bytes(files: &[(&str, &[u8])]) -> Self {
            let mut vault = MemoryVault::default();
            for (path, content) in files {
                vault.files.insert((*path).to_string(), Arc::from(*content));
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

        /// Count one listing of `folder`.
        fn listed(&self, folder: &str) {
            *self
                .listings
                .borrow_mut()
                .entry(folder.to_string())
                .or_default() += 1;
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
                        barrier: super::Barrier::Occupied,
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

        /// A memory vault excludes no place, so a control file is read
        /// where a document would be.
        fn control_entry(&self, path: &NormalizedPath) -> Result<Entry, Infallible> {
            self.entry(path)
        }

        fn folder_stands(&self, folder: &NormalizedPath) -> Result<bool, Infallible> {
            Ok(self.folder_spelling(folder).is_some())
        }

        fn folder_contents(
            &self,
            folder: &NormalizedPath,
        ) -> Result<Option<super::FolderContents>, Infallible> {
            if !self.folder_stands(folder)? {
                return Ok(None);
            }
            let depth = folder.as_path().components().count();
            let mut contents = super::FolderContents::default();
            for name in self.files.keys() {
                let run: PathBuf = Path::new(name).components().take(depth).collect();
                let below = Path::new(name).components().count() > depth;
                if below && self.identity(&run.to_string_lossy()) == *folder {
                    contents.take(Path::new(name));
                }
            }
            Ok(Some(contents))
        }

        fn visit_folder_names(
            &self,
            folder: &NormalizedPath,
            visit: &mut dyn FnMut(&OsStr) -> ControlFlow<()>,
        ) -> Result<(), Infallible> {
            self.listed(&folder.as_path().to_string_lossy());
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
            visited(names, visit);
            Ok(())
        }

        fn visit_root_names(
            &self,
            visit: &mut dyn FnMut(&OsStr) -> ControlFlow<()>,
        ) -> Result<(), Infallible> {
            self.listed("");
            let names: BTreeSet<OsString> = self
                .every_name()
                .filter_map(|name| {
                    Some(Path::new(name).components().next()?.as_os_str().to_owned())
                })
                .collect();
            visited(names, visit);
            Ok(())
        }
    }

    /// Hand `visit` each of `names`, in byte order, until it breaks.
    fn visited(names: BTreeSet<OsString>, visit: &mut dyn FnMut(&OsStr) -> ControlFlow<()>) {
        for name in names {
            if visit(&name).is_break() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
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
        let view = TreeView::open(scratch.root(), &[], &SchemaPlace::default()).expect("a vault");
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
        let Entry::Blocked { detail, .. } = entry(&view, "folder/a.md/under.md") else {
            panic!("nothing is made beneath a document");
        };
        assert!(detail.contains("beneath"), "{detail}");
        let Entry::Blocked { detail, .. } = entry(&view, "link/a.md") else {
            panic!("nothing is read through a link");
        };
        assert!(detail.contains("symbolic link"), "{detail}");
    }

    /// **A control file is read at the path the host reads it at, though
    /// the vault's walk does not enter it**: the schema's path, excluded as
    /// the host excludes it, reads as a document nowhere and as the control
    /// file it holds; an absent config reads as absent; and a folder at a
    /// control file's path is a place no control file can be written.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the tree a case reads.
    fn the_tree_view_reads_a_control_file_the_walk_does_not_enter() {
        let scratch = Scratch::new("planner-view-control");
        std::fs::create_dir_all(scratch.join(".norn")).expect("the control folder");
        std::fs::write(scratch.join(".norn/schema.yaml"), "version: 1\n").expect("a schema");
        let view = TreeView::open(
            scratch.root(),
            &[PathBuf::from(".norn/schema.yaml")],
            &SchemaPlace::default(),
        )
        .expect("a vault");
        let schema = view
            .normalizer()
            .normalize(Path::new(".norn/schema.yaml"))
            .expect("a vault path");
        assert!(matches!(
            view.entry(&schema).expect("a readable tree"),
            Entry::Blocked {
                barrier: Barrier::Closed,
                ..
            }
        ));
        let Entry::Document { at, bytes, hash } =
            view.control_entry(&schema).expect("a readable tree")
        else {
            panic!("the schema reads as the control file it is");
        };
        assert_eq!(at.as_str(), ".norn/schema.yaml");
        assert_eq!(&*bytes, b"version: 1\n");
        assert_eq!(hash, wire_hash(norn_fs::ContentHash::of(b"version: 1\n")));
        let config = view
            .normalizer()
            .normalize(Path::new(".norn/config.toml"))
            .expect("a vault path");
        assert!(matches!(
            view.control_entry(&config).expect("a readable tree"),
            Entry::Absent { at } if at.as_str() == ".norn/config.toml"
        ));
        std::fs::create_dir(scratch.join(".norn/config.toml")).expect("a folder in the way");
        assert!(matches!(
            view.control_entry(&config).expect("a readable tree"),
            Entry::Blocked {
                barrier: Barrier::Occupied,
                ..
            }
        ));
    }

    /// **The schema's role is read where the registration reads it** (ADR
    /// 0034), and answered at the role's path: a `schema_source` inside the
    /// vault at its own path, one outside it in its folder, while the
    /// default path holds other bytes; and a source naming no file, or one
    /// whose folder the shadow home cannot publish into, is a place no
    /// control file is written, naming why.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the tree a case reads.
    fn the_tree_view_reads_the_schema_where_the_registration_reads_it() {
        let scratch = Scratch::new("planner-view-schema-place");
        let vault = scratch.join("vault");
        std::fs::create_dir_all(vault.join(".norn")).expect("the control folder");
        std::fs::create_dir_all(vault.join("schemas")).expect("a schema folder");
        std::fs::write(vault.join(".norn/schema.yaml"), "default\n").expect("a default");
        std::fs::write(vault.join("schemas/notes.yaml"), "inside\n").expect("a source");
        let shared = scratch.join("shared");
        std::fs::create_dir_all(&shared).expect("a shared folder");
        std::fs::write(shared.join("schema.yaml"), "outside\n").expect("a source");
        let shadows = norn_fs::ShadowHome::resolve(
            &vault,
            &scratch.join("data/tmp"),
            &norn_fs::MaintainershipKey::new("norn", "notes", "base").expect("a key"),
        )
        .expect("a shadow home");
        let read = |place: &SchemaPlace| {
            let view = TreeView::open(&vault, &[], place).expect("a vault");
            let schema = view
                .normalizer()
                .normalize(Path::new(".norn/schema.yaml"))
                .expect("a vault path");
            view.control_entry(&schema).expect("a readable tree")
        };
        let outside = |folder: &Path| SchemaPlace::Outside {
            folder: folder.to_owned(),
            name: PathBuf::from("schema.yaml"),
            shadows: shadows.clone(),
        };
        for (place, holds) in [
            (SchemaPlace::default(), "default\n"),
            (
                SchemaPlace::InVault(PathBuf::from("schemas/notes.yaml")),
                "inside\n",
            ),
            (outside(&shared), "outside\n"),
        ] {
            let Entry::Document { at, bytes, .. } = read(&place) else {
                panic!("the schema stands at {place:?}");
            };
            assert_eq!(at.as_str(), ".norn/schema.yaml");
            assert_eq!(&*bytes, holds.as_bytes());
        }
        for (place, says) in [
            (SchemaPlace::NoFile(PathBuf::from("/")), "names no file"),
            (outside(&scratch.join("gone")), "cannot be written"),
        ] {
            let Entry::Blocked {
                detail,
                barrier: Barrier::Closed,
            } = read(&place)
            else {
                panic!("no schema is written at {place:?}");
            };
            assert!(detail.contains(says), "{detail}");
        }
    }

    #[test]
    fn the_tree_view_lists_a_folder_s_names() {
        let (_scratch, view) = tree();
        let folder = view
            .normalizer()
            .normalize(Path::new("folder"))
            .expect("a vault path");
        assert!(view.folder_stands(&folder).expect("a readable tree"));
        let mut names = Vec::new();
        view.visit_folder_names(&folder, &mut |name| {
            names.push(name.to_owned());
            ControlFlow::Continue(())
        })
        .expect("a readable tree");
        assert_eq!(names, vec![OsString::from("a.md")]);
    }

    #[test]
    fn the_tree_view_lists_the_root_s_names() {
        let (_scratch, view) = tree();
        let mut names = Vec::new();
        view.visit_root_names(&mut |name| {
            names.push(name.to_owned());
            ControlFlow::Continue(())
        })
        .expect("a readable tree");
        names.sort();
        assert_eq!(
            names,
            vec![OsString::from("folder"), OsString::from("link")]
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
        let view = TreeView::open(scratch.root(), &[], &SchemaPlace::default()).expect("a vault");
        if view.normalizer().case_sensitivity() != norn_fs::CaseSensitivity::Insensitive {
            return;
        }
        let Entry::Document { at, .. } = entry(&view, "note.md") else {
            panic!("the document stands at its listed spelling");
        };
        assert_eq!(at.as_str(), "Note.md");
    }
}
