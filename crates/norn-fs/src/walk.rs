#![forbid(unsafe_code)]
//! A streaming inventory of one vault tree.
//!
//! The walk keeps one bounded, sorted directory page per depth rather than a
//! tree of every path. That makes its memory a function of directory depth and
//! the fixed page size, not of the number of documents in the vault or one
//! directory's fan-out. Files and typed skip notations are yielded in
//! normalized-path lexicographic order; directories themselves are traversal
//! machinery and are not facts.
//!
//! Symbolic links are facts about names, never traversal edges. Host exclusions
//! are roots, not schema rules, and Norn's cross-device shadow fallback is the
//! one built-in mechanism root. Ordinary dotfiles and dot-directories belong to
//! the vault and are walked normally.
//!
//! A walk is always identified by the vault root, whether it traverses the whole
//! vault or one subtree of it. Paths, exclusion membership and link containment
//! are therefore vault-relative in both, and a subtree walk of `notes` reads the
//! same files the vault walk reads under `notes`.
//!
//! **The windows between the walk's own observations converge on absence.** A
//! walk observes a name more than once — the listing that names it, the stat of
//! the name that listing returned, the open or the readlink that follows that
//! stat, and, for a file, the open its consumer makes of the fact. Another
//! writer can unlink the name — or unlink it and give the name to something else
//! — inside any of those pairs, and one doctrine answers all of them: the walk
//! read nothing at that name, because a walk begun now holds nothing there
//! either. It says so, as [`SkipReason::Vanished`], rather than refusing or
//! staying silent: a name the walk read nothing at is a name whose derived state
//! this walk earned no authority over. A machine that will not answer is the
//! other thing entirely, and still refuses.
//!
//! The one observation with no such window before it is the open of the vault
//! root itself. A root that is not there is the vault gone rather than one name
//! inside it changing, and it refuses.

mod faults;

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use rustix::fs::{AtFlags, Dir, FileType, Mode, open, openat, readlinkat, statat};

use self::faults::{Paged, WalkFaults};
use crate::exclusion::{Excluded, ExclusionError, Exclusions};
use crate::hash::{ContentHash, read_bytes_and_hash};
use crate::identity::{Identity, identity_of};
use crate::open::{Reached, directory_flags, open_regular_at};
use crate::path::{ChildKeys, NormalizedPath, NormalizerError, PathError, PathNormalizer};

/// Begins a deterministic streaming walk of `root`.
///
/// Construction proves the root's case behavior, validates every exclusion,
/// and opens only the first directory frontier. Later directory failures arrive
/// from the iterator at their deterministic position. The one failure whose
/// position the listing order moves is a refusing stat on a filesystem whose
/// listing carries no entry type, and the page states it.
pub fn walk(root: &Path, exclusions: &[PathBuf]) -> Result<Walk, WalkError> {
    let vault = Vault::open(root, exclusions)?;
    let first = DirectoryFrontier::new(vault.root_fd.clone(), Path::new(""));
    let mut walk = vault.into_walk();
    walk.stack.push(first);
    Ok(walk)
}

/// Begins a deterministic streaming walk of the `relative` subtree of `root`.
///
/// The walk is the vault's, narrowed to one subtree: facts carry vault-relative
/// paths, exclusion roots are the vault's own, and a link's containment is
/// judged against the vault root. A subtree the vault walk does not descend to
/// is not entered and yields exactly the skip the vault walk states at the
/// component that stops it.
pub fn walk_subtree(
    root: &Path,
    relative: &Path,
    exclusions: &[PathBuf],
) -> Result<Walk, WalkError> {
    let vault = Vault::open(root, exclusions)?;
    let subtree = vault.normalize(relative)?;
    if let Some(reason) = vault.exclusions.reason(&subtree) {
        let mut walk = vault.into_walk();
        walk.skipped = Some(SkipFact {
            path: subtree,
            reason: reason.into(),
        });
        return Ok(walk);
    }
    let frontier = open_subtree(&vault, &subtree)?;
    let mut walk = vault.into_walk();
    match frontier {
        Frontier::Open(fd) => walk
            .stack
            .push(DirectoryFrontier::new(fd, subtree.as_path())),
        Frontier::Skipped(skipped) => walk.skipped = Some(skipped),
    }
    Ok(walk)
}

/// One vault, opened: its root, a descriptor on that root, the case behavior
/// the root proved, and the roots inside it Norn does not read.
///
/// A [`Walk`] is one of these with a frontier on it, and
/// [`skip_reaching`](Self::skip_reaching) is the same descent carrying no
/// frontier at all. Opening proves the root's case behavior once, so a caller
/// deciding many names against one vault pays for that proof once rather than
/// per name.
pub struct Vault {
    root: Arc<PathBuf>,
    root_fd: Arc<OwnedFd>,
    normalizer: PathNormalizer,
    exclusions: Exclusions,
}

impl Vault {
    /// Proves the root's case behavior, validates every exclusion, and opens
    /// the root. Nothing below the root is read.
    pub fn open(root: &Path, exclusions: &[PathBuf]) -> Result<Self, WalkError> {
        let normalizer = PathNormalizer::detect(root).map_err(WalkError::Normalizer)?;
        let exclusions = Exclusions::new(&normalizer, exclusions)?;
        let root_fd = Arc::new(
            open(root, directory_flags(), Mode::empty())
                .map_err(|source| environment_errno("opening directory", root, source))?,
        );
        Ok(Self {
            root: Arc::new(root.to_owned()),
            root_fd,
            normalizer,
            exclusions,
        })
    }

    /// The path ordering proven for this root.
    pub fn case_sensitivity(&self) -> crate::CaseSensitivity {
        self.normalizer.case_sensitivity()
    }

    /// The notation the vault walk states in place of reaching `relative`, or
    /// nothing where the walk reaches that name.
    ///
    /// This is [`walk_subtree`]'s descent with no frontier on the end of it,
    /// read in the descent's own order: an exclusion root covering the path,
    /// then each name above the last one decided the way the walk decides the
    /// entries it descends through, then the last name's own spelling. So a
    /// path under a link is the link's answer rather than its own leaf's, which
    /// is the order the walk reaches the two names in.
    ///
    /// **The notation stands at the root the walk stops at**, not at the path
    /// that reached it: the excluded root, the shadow basename, the link. Every
    /// place under that root holds the same nothing, so a consumer converging
    /// derived state reads the root once for all of them —
    /// [`SkipFact::covers`] is how it asks which later paths that reading
    /// already answered.
    ///
    /// **What the last name holds is not read here.** A caller that must tell a
    /// file from a directory from an absence asks [`crate::path_kind`] for it,
    /// and this leaves that one reading whole rather than taking half of it.
    ///
    /// **A name that is not there is not a refusal.** A path no name leads to
    /// holds nothing, which is what `path_kind` answers at it, so this states
    /// no notation and lets that reading stand alone. A path a *name* blocks is
    /// the other case and is stated here, as [`SkipReason::UnderAnEntry`]:
    /// `path_kind` cannot answer at all through an entry that is not a
    /// directory.
    ///
    /// **A notation is a fact about an entry**, so the descent proves each name
    /// above the last one is there before it reads that name as a shadow
    /// basename or as a link. A spelling nothing stands at vanishes on any
    /// root, and the last name's spelling is reached only once every name above
    /// it is there — so an absent ancestor answers, and a shadow basename below
    /// one is never spelled at all.
    pub fn skip_reaching(&self, relative: &Path) -> Result<Option<SkipFact>, WalkError> {
        let subtree = self.normalize(relative)?;
        if let Some((root, reason)) = self.exclusions.covering_root(&subtree) {
            return Ok(Some(SkipFact {
                path: root.clone(),
                reason: reason.into(),
            }));
        }
        let above = subtree.as_path().parent().unwrap_or(Path::new(""));
        let mut directory = self.root_fd.clone();
        let mut traversed = PathBuf::new();
        for component in above.components() {
            let name = component.as_os_str();
            traversed.push(name);
            let access = self.root.join(&traversed);
            let path = self.traversed(&traversed);
            match pass_component(self, &directory, name, &path, &access)? {
                Passed::Skipped(reason) => return Ok(Some(SkipFact { path, reason })),
                Passed::Vanished => return Ok(None),
                Passed::NotADirectory => {
                    return Ok(Some(SkipFact {
                        path: subtree,
                        reason: SkipReason::UnderAnEntry,
                    }));
                }
                Passed::Directory => {}
            }
            directory = match open_component(&directory, name, &access)? {
                Some(fd) => fd,
                None => return Ok(None),
            };
        }
        let Some(leaf) = subtree.as_path().file_name() else {
            return Ok(None);
        };
        if self.names_a_shadow(leaf) {
            return Ok(Some(SkipFact {
                path: subtree,
                reason: SkipReason::Shadow,
            }));
        }
        Ok(None)
    }

    /// Whether `name` is one of Norn's shadow basenames on this root.
    fn names_a_shadow(&self, name: &OsStr) -> bool {
        self.normalizer.names_a_shadow(name)
    }

    /// This vault's identity for `relative`, or why that spelling names nothing
    /// inside it.
    fn normalize(&self, relative: &Path) -> Result<NormalizedPath, WalkError> {
        self.normalizer
            .normalize(relative)
            .map_err(|source| WalkError::Path {
                path: relative.to_owned(),
                source,
            })
    }

    /// The identity of a leading run of an already normalized path.
    fn traversed(&self, traversed: &Path) -> NormalizedPath {
        self.normalizer
            .normalize(traversed)
            .expect("a leading run of a normalized path's components is normalized")
    }

    /// This vault carrying a walk's frontier state, with no frontier on it yet.
    fn into_walk(self) -> Walk {
        Walk {
            vault: self,
            stack: Vec::new(),
            skipped: None,
            faults: WalkFaults::entry(),
        }
    }
}

/// The subtree frontier, or the skip that stands in place of it.
enum Frontier {
    Open(Arc<OwnedFd>),
    Skipped(SkipFact),
}

/// Descends to the subtree one component at a time, deciding each component the
/// way the vault walk decides the entries it descends through: a name the vault
/// walk skips ends the descent at that name's own skip, so a subtree walk reads
/// nothing under a name the vault walk never enters.
///
/// A single multi-component open would resolve the intermediate names in the
/// kernel, where `O_NOFOLLOW` binds only the last of them.
///
/// **A component that is not there ends the descent at that name's own
/// vanishing**, the way every other window between two observations of a name
/// ends. The caller reached this subtree from an observation of its own, and a
/// writer editing the vault between that observation and this descent is the
/// vault evolving: the walk reads nothing under the name and says so, so what is
/// stored beneath it converges while the findings there stay withheld.
fn open_subtree(vault: &Vault, subtree: &NormalizedPath) -> Result<Frontier, WalkError> {
    let mut directory = vault.root_fd.clone();
    let mut traversed = PathBuf::new();
    for component in subtree.as_path().components() {
        let name = component.as_os_str();
        traversed.push(name);
        let access = vault.root.join(&traversed);
        let path = vault.traversed(&traversed);
        let vanished = SkipFact {
            path: path.clone(),
            reason: SkipReason::Vanished,
        };
        match pass_component(vault, &directory, name, &path, &access)? {
            Passed::Skipped(reason) => return Ok(Frontier::Skipped(SkipFact { path, reason })),
            // A frontier is a directory or it is nothing, so an entry standing
            // where one was named leaves this walk the same nothing an absence
            // does.
            Passed::Vanished | Passed::NotADirectory => {
                return Ok(Frontier::Skipped(vanished));
            }
            Passed::Directory => {}
        }
        directory = match open_component(&directory, name, &access)? {
            Some(fd) => fd,
            None => return Ok(Frontier::Skipped(vanished)),
        };
    }
    Ok(Frontier::Open(directory))
}

/// What a descent meets at one name it passes through.
enum Passed {
    /// A directory the descent continues through.
    Directory,
    /// The notation the walk states at this name in place of descending it.
    Skipped(SkipReason),
    /// No name is there.
    Vanished,
    /// An entry the walk reads rather than descends into. Nothing the descent
    /// is reaching for stands below it.
    NotADirectory,
}

/// Reads one name a descent passes through, the way the page reads the entries
/// it descends through.
///
/// **The stat comes first, so every verdict here is a verdict about an entry.**
/// The page judges names a directory stream handed it, and each of those is a
/// name something stands at. This descent is handed a caller's spelling
/// instead, so it proves an entry is there before it reads the name as a Norn
/// shadow basename or as a symbolic link. A spelling nothing stands at is an
/// absence on any root, and a root that tells two spellings apart holds the
/// shadow name and an ordinary neighbour as two separate names.
///
/// A Norn shadow basename is never entered, and a symbolic link is a fact about
/// a name rather than an edge to follow. Absence and a non-directory entry are
/// kept apart because they are two different answers about what stands below:
/// nothing is under a name that is not there, and nothing is under an entry the
/// walk reads either — but only the second is a name a caller can be told
/// about.
#[allow(clippy::disallowed_methods)] // norn-fs owns the vault walk and stat.
fn pass_component(
    vault: &Vault,
    directory: &Arc<OwnedFd>,
    name: &OsStr,
    path: &NormalizedPath,
    access: &Path,
) -> Result<Passed, WalkError> {
    crate::reads::count_stat();
    let metadata = match statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => metadata,
        Err(rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR) => return Ok(Passed::Vanished),
        Err(source) => return Err(environment_errno("stating", access, source)),
    };
    if vault.names_a_shadow(name) {
        return Ok(Passed::Skipped(SkipReason::Shadow));
    }
    Ok(
        match classify_file_type(FileType::from_raw_mode(metadata.st_mode as _)) {
            EntryKind::Directory => Passed::Directory,
            EntryKind::File | EntryKind::Special(_) => Passed::NotADirectory,
            EntryKind::Symlink => match classify_link(
                &vault.root_fd,
                path.as_path(),
                directory,
                name,
                &vault.normalizer,
            )? {
                Some(kind) => Passed::Skipped(SkipReason::SymbolicLink(kind)),
                None => Passed::Vanished,
            },
        },
    )
}

/// Opens one name of a descent as the next directory, or answers that there is
/// no directory at it to open.
fn open_component(
    directory: &Arc<OwnedFd>,
    name: &OsStr,
    access: &Path,
) -> Result<Option<Arc<OwnedFd>>, WalkError> {
    match openat(directory, name, directory_flags(), Mode::empty()) {
        Ok(fd) => Ok(Some(Arc::new(fd))),
        Err(rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP) => {
            Ok(None)
        }
        Err(source) => Err(environment_errno("opening directory", access, source)),
    }
}

/// A streaming vault walk.
///
/// Any yielded error is terminal: later calls return `None`, so exhaustion can
/// describe a complete inventory only when every preceding item was `Ok`.
pub struct Walk {
    vault: Vault,
    stack: Vec<DirectoryFrontier>,
    skipped: Option<SkipFact>,
    /// Which of this walk's paging observations a foreign writer's edit stands
    /// in place of. Empty in every process that armed nothing, which is every
    /// process outside the induced-failure suites.
    faults: WalkFaults,
}

impl Iterator for Walk {
    type Item = Result<WalkFact, WalkError>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(skipped) = self.skipped.take() {
            return Some(Ok(WalkFact::Skipped(skipped)));
        }
        loop {
            let frontier = self.stack.last_mut()?;
            let pending = match frontier.next_pending(
                &self.vault.root_fd,
                &self.vault.normalizer,
                &self.vault.exclusions,
                &self.faults,
            ) {
                Ok(Some(pending)) => pending,
                Ok(None) => {
                    self.stack.pop();
                    continue;
                }
                Err(error) => {
                    self.stack.clear();
                    return Some(Err(error));
                }
            };

            match pending.observed {
                Observed::Skipped(reason) => {
                    return Some(Ok(WalkFact::Skipped(SkipFact {
                        path: pending.path,
                        reason,
                    })));
                }
                Observed::File(stat) => {
                    return Some(Ok(WalkFact::File(FileFact {
                        root: self.vault.root.clone(),
                        root_fd: self.vault.root_fd.clone(),
                        path: pending.path,
                        stat,
                    })));
                }
                // **The page's stat and this open are two observations of the
                // same name**, and it is the widest of the walk's windows: a
                // page is stat'd whole and its entries are handed out one at a
                // time, so a directory late in a page is opened only after every
                // earlier entry — whole subtrees included — has been read. A
                // writer removing the directory in that span, or replacing it
                // with something that is not one, leaves nothing here to enter,
                // which is the answer a walk begun now gives at that name.
                Observed::Directory => {
                    let access = self.vault.root.join(pending.path.as_path());
                    match openat(
                        &pending.parent,
                        &pending.name,
                        directory_flags(),
                        Mode::empty(),
                    ) {
                        Ok(fd) => self
                            .stack
                            .push(DirectoryFrontier::new(Arc::new(fd), pending.path.as_path())),
                        Err(
                            rustix::io::Errno::NOENT
                            | rustix::io::Errno::NOTDIR
                            | rustix::io::Errno::LOOP,
                        ) => {
                            return Some(Ok(WalkFact::Skipped(SkipFact {
                                path: pending.path,
                                reason: SkipReason::Vanished,
                            })));
                        }
                        Err(source) => {
                            self.stack.clear();
                            return Some(Err(environment_errno(
                                "opening directory",
                                &access,
                                source,
                            )));
                        }
                    }
                }
            }
        }
    }
}

impl Walk {
    /// The path ordering proven for this walk's root.
    pub fn case_sensitivity(&self) -> crate::CaseSensitivity {
        self.vault.case_sensitivity()
    }
}

/// One filesystem fact from a walk.
#[derive(Debug)]
pub enum WalkFact {
    /// A non-directory, non-symlink entry, whatever its extension or bytes.
    File(FileFact),
    /// One root Norn deliberately did not enter or read.
    Skipped(SkipFact),
}

impl WalkFact {
    /// The normalized vault-relative identity of this fact.
    pub fn path(&self) -> &NormalizedPath {
        match self {
            Self::File(file) => file.path(),
            Self::Skipped(skip) => skip.path(),
        }
    }
}

/// A file observed during traversal, before its content is requested.
#[derive(Debug)]
pub struct FileFact {
    root: Arc<PathBuf>,
    root_fd: Arc<OwnedFd>,
    path: NormalizedPath,
    stat: FileStat,
}

impl FileFact {
    /// The normalized vault-relative identity.
    pub fn path(&self) -> &NormalizedPath {
        &self.path
    }

    /// The cheap traversal-time observation. It may prioritize; it concludes
    /// nothing about content.
    pub fn stat(&self) -> &FileStat {
        &self.stat
    }

    /// Opens once, reads the bytes once, and hashes exactly those returned
    /// bytes — or answers that there is no file at this name to read.
    ///
    /// Consuming the fact makes a second read through this observation
    /// unspellable. The returned stat comes from the held descriptor and may
    /// differ from the traversal-time stat if another writer replaced the name.
    ///
    /// The path is resolved from the walk's own root descriptor, so a fact this
    /// walk yielded is read through the tree it walked.
    ///
    /// **A name that has stopped identifying a regular file since it was
    /// yielded is an answer, not a refusal.** Removed, replaced by a link, by a
    /// pipe or by a directory are one answer — *nothing to read here* — because
    /// a walk begun now yields no file at that name in any of those cases
    /// either, so a caller converging on what the tree holds converges the same
    /// way for all of them. Enumeration and open are separate observations of a
    /// tree other writers are editing, and the window between them is ordinary
    /// churn rather than a broken environment.
    ///
    /// A machine failure is still a refusal. A directory this account cannot
    /// open and a descriptor table that is full say nothing about whether a
    /// document is there, and reporting them as absence would let a transient
    /// fault delete derived state.
    #[allow(clippy::disallowed_methods, clippy::disallowed_types)] // norn-fs owns vault handles and reads.
    pub fn read_optional(self) -> Result<Option<ReadFile>, WalkError> {
        let access = self.root.join(self.path.as_path());
        let fd = match open_regular_at(self.root_fd.as_fd(), self.path.as_path())
            .map_err(|source| environment(source.operation(), &access, source.into_error()))?
        {
            Reached::Regular(fd) => fd,
            Reached::Nothing(_) => return Ok(None),
        };
        let mut file = fs::File::from(fd);
        crate::reads::count_stat();
        let metadata = file
            .metadata()
            .map_err(|source| environment("stating", &access, source))?;
        let (bytes, content_hash) = read_bytes_and_hash(&mut file)
            .map_err(|source| environment("reading", &access, source))?;
        Ok(Some(ReadFile {
            path: self.path,
            stat: stat(&metadata),
            bytes,
            content_hash,
        }))
    }
}

/// Bytes and identity produced by one read of one held descriptor.
#[derive(Clone, Debug)]
pub struct ReadFile {
    path: NormalizedPath,
    stat: FileStat,
    bytes: Vec<u8>,
    content_hash: ContentHash,
}

impl ReadFile {
    /// The normalized vault-relative identity whose bytes were read.
    pub fn path(&self) -> &NormalizedPath {
        &self.path
    }

    /// The identity observed through the held file descriptor.
    pub fn stat(&self) -> &FileStat {
        &self.stat
    }

    /// The bytes read in the single filesystem pass.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The SHA-256 computed from exactly [`Self::bytes`].
    pub fn content_hash(&self) -> ContentHash {
        self.content_hash
    }

    /// Splits the observation into the bytes parsers consume and their hash.
    pub fn into_parts(self) -> (Vec<u8>, ContentHash, FileStat) {
        (self.bytes, self.content_hash, self.stat)
    }
}

/// The stat fields the walk's consumers actually use.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileStat {
    pub len: u64,
    pub mtime: SystemTime,
    pub identity: Identity,
}

/// One root the walk read nothing under, and why.
///
/// Most are roots Norn deliberately does not enter. One is not: a name that went
/// away inside one of the walk's own windows is a root nothing here read either,
/// and it carries the same notation so a consumer holds one rule for both.
#[derive(Clone, Debug)]
pub struct SkipFact {
    path: NormalizedPath,
    reason: SkipReason,
}

impl SkipFact {
    /// The normalized vault-relative root that was skipped.
    pub fn path(&self) -> &NormalizedPath {
        &self.path
    }

    /// Why this root has no descendant facts.
    pub fn reason(&self) -> SkipReason {
        self.reason
    }

    /// Whether `path` is this root or lies beneath it.
    ///
    /// Containment is answered on the vault's proven case behavior, the way
    /// every identity question on that root is. A consumer holding many paths
    /// asks it so that one reading of this root answers for every path the root
    /// already covers.
    pub fn covers(&self, path: &NormalizedPath) -> bool {
        path.starts_with(&self.path)
    }
}

/// Why a root has one notation and no descendant facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkipReason {
    /// A root supplied by the host.
    HostExclusion,
    /// Norn's exact `.norn/tmp` fallback subtree.
    Mechanism,
    /// A Norn shadow basename, read under the root's own case behavior.
    Shadow,
    /// A symbolic link, which is never traversed.
    SymbolicLink(LinkKind),
    /// A device-like entry unsafe to open as document content.
    SpecialFile(FileKind),
    /// A name below an entry the walk reads rather than descends into.
    ///
    /// The walk yields that entry — a document, or its own notation at a name
    /// that is neither file nor directory — and nothing beneath it. So this
    /// root is a spelling the vault holds nothing at, and holds nothing at for
    /// as long as that entry stands.
    ///
    /// [`Vault::skip_reaching`] is what states it, for a caller deciding one
    /// name it never enumerated. A walk carries it in no fact of its own: a
    /// frontier is a directory or it is nothing, so a subtree walk named
    /// through such an entry reads nothing at the name and says
    /// [`SkipReason::Vanished`] there.
    UnderAnEntry,
    /// A name another writer took away — or took away and gave to something
    /// else — between two of this walk's observations of it.
    ///
    /// **The walk read nothing at the name**, which is the whole of what this
    /// says. What is stored under it converges on the answer a walk begun now
    /// holds, and a consumer that concludes from enumeration owes this root the
    /// same hold it owes every other one here: nothing this walk did earns
    /// authority over the places beneath a name it never read.
    Vanished,
}

impl From<Excluded> for SkipReason {
    fn from(excluded: Excluded) -> Self {
        match excluded {
            Excluded::Host => Self::HostExclusion,
            Excluded::Mechanism => Self::Mechanism,
        }
    }
}

/// What an unsupported symbolic link was observed to name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkKind {
    /// A direct, contained target that is a regular or special file.
    InVaultFile,
    /// A direct, contained directory target.
    InVaultDirectory,
    /// An absent target or a target path containing another symbolic link.
    Dangling,
    /// A lexical target outside the vault root.
    Outbound,
}

/// A filesystem entry that is neither a document-like file nor a directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileKind {
    /// A named pipe.
    Fifo,
    /// A Unix-domain socket.
    Socket,
    /// A block device.
    BlockDevice,
    /// A character device.
    CharacterDevice,
    /// Another platform-specific special kind.
    Other,
}

/// A walk or content-read failure, always naming the affected path.
#[derive(Debug)]
pub enum WalkError {
    Normalizer(NormalizerError),
    Path {
        path: PathBuf,
        source: PathError,
    },
    Environment {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
}

impl From<ExclusionError> for WalkError {
    fn from(error: ExclusionError) -> Self {
        Self::Path {
            path: error.path,
            source: error.source,
        }
    }
}

impl fmt::Display for WalkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Normalizer(error) => error.fmt(formatter),
            Self::Path { path, source } => {
                write!(formatter, "cannot normalize {}: {source}", path.display())
            }
            Self::Environment {
                operation,
                path,
                source,
            } => write!(formatter, "{operation} {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for WalkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Normalizer(error) => Some(error),
            Self::Path { source, .. } => Some(source),
            Self::Environment { source, .. } => Some(source),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryKind {
    Directory,
    File,
    Symlink,
    Special(FileKind),
}

struct Pending {
    parent: Arc<OwnedFd>,
    name: OsString,
    path: NormalizedPath,
    observed: Observed,
}

/// What the page's stat of one listed name found there.
///
/// Every name a page covers gets one of these, so the page hands back as many
/// entries as it took in and a name that went away is an entry saying so rather
/// than a gap.
enum Observed {
    /// A directory the walk descends into.
    Directory,
    /// A file, with the stat the page took of it.
    File(FileStat),
    /// A name the walk states a notation for instead of reading.
    Skipped(SkipReason),
}

struct Candidate {
    name: OsString,
    path: NormalizedPath,
    /// The kind the listing named, or nothing where the name was already gone
    /// when the stat that learned its kind ran.
    listed: Option<EntryKind>,
    sort_key: Vec<u8>,
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.sort_key.cmp(&other.sort_key)
    }
}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.sort_key == other.sort_key
    }
}

impl Eq for Candidate {}

const FRONTIER_PAGE: usize = 256;

/// The lowest-keyed run of a directory's names one page is choosing from.
///
/// It holds `FRONTIER_PAGE + 1` names: the page's own entries, and one more
/// whose presence is the answer to whether the directory holds anything past
/// them. A max-heap, because the question the listing asks of it once per name
/// is "can this name still get in", and the highest key it holds is what
/// answers that in one comparison.
struct Run {
    kept: std::collections::BinaryHeap<Candidate>,
}

impl Run {
    const CAPACITY: usize = FRONTIER_PAGE + 1;

    fn new() -> Run {
        Run {
            kept: std::collections::BinaryHeap::with_capacity(Self::CAPACITY + 1),
        }
    }

    /// Whether a name whose sort key is at least `floor` can still enter.
    ///
    /// The caller has a lower bound on the name's sort key before it has the
    /// name's identity, and that is enough: a run already holding
    /// `CAPACITY` names at or below that bound cannot take another.
    fn reaches(&self, floor: &[u8]) -> bool {
        self.kept.len() < Self::CAPACITY
            || self
                .kept
                .peek()
                .is_some_and(|highest| floor < highest.sort_key.as_slice())
    }

    fn offer(&mut self, candidate: Candidate) {
        self.kept.push(candidate);
        if self.kept.len() > Self::CAPACITY {
            self.kept.pop();
        }
    }

    /// The run in ascending sort-key order.
    fn ascending(self) -> Vec<Candidate> {
        self.kept.into_sorted_vec()
    }
}

/// Whether a name whose comparison key is `key` can sort past `cursor`.
///
/// A listed name's sort key is its own comparison key, or that key and a
/// separator where the name is a directory the walk descends into — so the key
/// alone is the lowest sort key the name can carry. Both spellings pass a
/// cursor the key itself passes. Where the key does not, only the separator
/// spelling can, and only while the cursor runs on past the key with a byte
/// below the separator; that is at most a handful of the directory's names, and
/// they are the only ones this cannot settle without their kind.
fn can_sort_past(key: &[u8], cursor: &[u8]) -> bool {
    key > cursor
        || (cursor.starts_with(key) && cursor.get(key.len()).is_none_or(|byte| *byte < b'/'))
}

/// Whether a listed name whose comparison key is `key` is worth an identity.
///
/// Two rejections are settled on the key alone, before the name has cost
/// anything else: a name that cannot sort past `cursor` is one an earlier page
/// already handed out, and a name `run` cannot reach is one this page is already
/// full past. Neither can enter the page, so what this answers is what the page
/// spends, never what it holds.
fn worth_identifying(run: &Run, cursor: Option<&[u8]>, key: &[u8]) -> bool {
    cursor.is_none_or(|cursor| can_sort_past(key, cursor)) && run.reaches(key)
}

struct DirectoryFrontier {
    fd: Arc<OwnedFd>,
    relative: PathBuf,
    cursor: Option<Vec<u8>>,
    page: std::vec::IntoIter<Pending>,
    done: bool,
    #[cfg(test)]
    max_page: usize,
    /// How many of the names its listings returned this frontier built an
    /// identity for — the normalized path, the kind the entry holds, and the
    /// sort key the two decide. It is the per-name work a page spends, and it
    /// is what the listing count above it is not: a page lists the whole
    /// directory and identifies only the run it can still reach.
    #[cfg(test)]
    identified: u64,
}

impl DirectoryFrontier {
    fn new(fd: Arc<OwnedFd>, relative: &Path) -> Self {
        Self {
            fd,
            relative: relative.to_owned(),
            cursor: None,
            page: Vec::new().into_iter(),
            done: false,
            #[cfg(test)]
            max_page: 0,
            #[cfg(test)]
            identified: 0,
        }
    }

    /// The next entry this directory holds, paging the directory again where the
    /// page in hand is spent.
    ///
    /// **Every name a page covers comes back as an entry**, so a spent page is a
    /// spent run of names and the cursor it leaves is the last of them. A name
    /// another writer took away inside the page's own window comes back as the
    /// notation saying so rather than as nothing, which is what keeps the run the
    /// page covered and the entries it hands out one list.
    fn next_pending(
        &mut self,
        root_fd: &Arc<OwnedFd>,
        normalizer: &PathNormalizer,
        exclusions: &Exclusions,
        faults: &WalkFaults,
    ) -> Result<Option<Pending>, WalkError> {
        if let Some(item) = self.page.next() {
            return Ok(Some(item));
        }
        if self.done {
            return Ok(None);
        }
        let page = match directory_page(
            root_fd,
            &self.fd,
            &self.relative,
            self.cursor.as_deref(),
            normalizer,
            exclusions,
            faults,
        ) {
            Ok(page) => page,
            Err(error) => {
                self.done = true;
                return Err(error);
            }
        };
        self.done = !page.more;
        #[cfg(test)]
        {
            self.max_page = self.max_page.max(page.pending.len());
            self.identified += page.identified;
        }
        if let Some(covered) = page.covered {
            self.cursor = Some(covered);
        }
        self.page = page.pending.into_iter();
        Ok(self.page.next())
    }
}

/// One bounded page of a directory's entries.
struct Page {
    /// The entries the page holds, in order — one per name it covered.
    pending: Vec<Pending>,
    /// Whether the directory holds names past the ones this page covered.
    more: bool,
    /// The highest sort key this page covered, which is where the next page of
    /// the same directory starts.
    covered: Option<Vec<u8>>,
    /// How many listed names this page built an identity for.
    #[cfg(test)]
    identified: u64,
}

/// One page of `directory`, listed and then stat'd.
///
/// **The listing and the stats are two observations, and the page converges on
/// what the second one holds.** A name the listing returned can be unlinked
/// before its stat runs, and it can be unlinked and taken by an entry of another
/// kind; both are one edit to a tree other writers share, and both are answered
/// the same way — the page read nothing at that name, and states it as
/// [`SkipReason::Vanished`]. That is the answer a walk begun now gives, since it
/// lists no such entry, so a heal reading this page converges on the same state
/// either way.
///
/// **A name stated that way is a name this walk earned nothing over.** The page
/// says so rather than passing the name over in silence, because a consumer that
/// prunes what its enumeration did not account for would otherwise read the
/// silence as a name it had read and found empty. What stands at the name now —
/// the successor of a replacement, or nothing at all — is not this page's to
/// describe: it is a change of the vault's own, reached by the watcher delivery
/// that edit raises or by the next heal that reads the name.
///
/// **A machine that will not answer still refuses.** A denied stat, an exhausted
/// descriptor table and a failing device say nothing about whether an entry is
/// there, and reading one as absence would let a transient fault prune derived
/// state. Absence is the one error number that converges; every other one ends
/// the walk.
///
/// # Every page lists the whole directory, and only the cursor is spared
///
/// A directory stream returns names in no order, so a page that yields the
/// lowest keys the directory still holds has to read every name to find them.
/// Carrying names forward in memory is what would read fewer, and the
/// frontier's memory bound forbids it: a walk's memory is a function of depth
/// and the fixed page size, never of one directory's fan-out. **The listing cost
/// of a wide directory is therefore quadratic in its width, and that is accepted
/// rather than overlooked**: the bar it is accepted up to is authored in this
/// module's own suite, at a stated width, over the dirents a walk reads — and so
/// is the width past which the shape has to change instead.
///
/// **A sorted listing spilled to a file and paged back from would read each name
/// once and keep the memory bound too, so the bound is not what rules it out.**
/// Two properties of a walk are. The first is that a walk takes nothing on the
/// vault: it opens, lists, stats and reads, and writes nowhere. A spill needs a
/// writable place — the vault, which puts the walk's own bookkeeping inside the
/// tree it is inventorying and into the changes the watcher delivers back, or
/// the mechanism root, which makes an inventory refuse on a vault the process
/// may read and may not write beside. A read that can fail for want of write
/// access is a failure class this path does not have. The second is that a
/// spilled listing is one observation replayed: today the names a page acts on
/// came off a stream that page drained itself, so the window between a listing
/// and the stats that answer it is one page wide, and every later page lists the
/// directory as it stands then. A replayed spill widens that window to the whole
/// walk of the directory. The doctrine above still converges every name in it,
/// but the number of names standing in the window goes from one page's worth to
/// the directory's width, and the walk reports reading nothing at names that
/// were there when it looked.
///
/// Those costs are worth paying at a width the rescan cannot serve, which is why
/// the bar states one. They are not worth paying at the width the bar accepts.
///
/// What the listing must not also cost is the *identity* of every name it
/// returns. A name at or below the cursor is one an earlier page already handed
/// out, and a name above every key the run in hand holds cannot get into this
/// one; both are settled on the name's comparison key, which is a fold into a
/// buffer this already owns. Only a name that passes both is worth a normalized
/// path, the stat a type-less listing owes it, and the sort key those decide.
/// On a filesystem whose listing carries no entry type, that stat is the whole
/// per-name cost, so what bounds the identities bounds the stats too.
///
/// That stat is also where such a filesystem meets a machine failure, and which
/// page meets it is the listing order's to decide: a refusing stat is reached by
/// the first page whose run still admits the name's key, which is the opening
/// page for whichever names the stream returns first and a later page for the
/// rest. The walk refuses either way and yields nothing at that name or after
/// it; what the order moves is how many facts the consumer already took when the
/// refusal arrives. A listing that carries entry types owes no such stat and has
/// no such spread.
///
/// # What resumes a directory is a key, not a kernel offset
///
/// Each page opens its own stream over the directory descriptor the frontier
/// holds and reads it from the top; nothing is carried between pages except the
/// highest sort key the last one covered. **No `telldir` cookie or stream
/// offset survives a page**, which is what makes the resumption mean the same
/// thing on every platform this crate builds for — Linux's `getdents64` and
/// macOS's `getdirentries` differ in how an offset behaves across a directory
/// edit, and neither answer is load-bearing here.
///
/// So a directory edited between two pages is answered by the key ordering
/// alone, identically on both, and there are four answers. A name created above
/// the cursor is listed by a later page and walked; a name created at or below
/// it is not seen by this walk, and reaches derived state through the watcher
/// delivery its creation raises or the next heal. A name removed between pages
/// is simply not listed again.
///
/// **The fourth is a name replaced in place by an entry of another kind, and it
/// is the one a single walk can report twice.** A page cannot reject a name
/// whose key runs *into* the cursor rather than past it — the cursor's own name,
/// or a name the cursor extends with a byte below the separator — since a
/// directory of that name sorts above the cursor where the bare name does not.
/// So a name an earlier page handed out as a document, standing as a directory
/// the walk descends into when a later page lists it, is listed again and sorts
/// above the cursor this time: the same walk yields the document at that name
/// and then the documents under it. Both are things this walk read: it read a
/// file there, and it read a tree there afterwards. Nothing prunes either on that
/// account, because the walk accounted for the name; the pair converges through
/// the watcher delivery the replacement raises or the next heal, the same way the
/// three above do. The replacement running the other way — a directory a page
/// descended into, standing as a document when the next page lists it — sorts
/// the survivor at or below the cursor the separator left, so that name is not
/// listed again and the walk reports it once.
///
/// What a single listing owes under a concurrent edit is the platform's
/// own — a name added or removed while one stream is being drained may or may
/// not appear — and that is the same window a walk of any shape stands in.
#[allow(clippy::disallowed_methods)] // norn-fs owns the vault walk and stat.
fn directory_page(
    root_fd: &Arc<OwnedFd>,
    directory: &Arc<OwnedFd>,
    relative: &Path,
    cursor: Option<&[u8]>,
    normalizer: &PathNormalizer,
    exclusions: &Exclusions,
    faults: &WalkFaults,
) -> Result<Page, WalkError> {
    let display = relative;
    let entries = Dir::read_from(directory)
        .map_err(|source| environment_errno("reading directory", display, source))?;
    let mut keys = ChildKeys::under(normalizer, relative);
    let mut run = Run::new();
    #[cfg(test)]
    let mut identified = 0;
    for entry in entries {
        let entry =
            entry.map_err(|source| environment_errno("reading entry in", display, source))?;
        let listed_name = entry.file_name().to_bytes();
        if listed_name == b"." || listed_name == b".." {
            continue;
        }
        crate::reads::count_dirents(1);
        let key = keys.of(listed_name);
        if !worth_identifying(&run, cursor, key) {
            continue;
        }
        #[cfg(test)]
        {
            identified += 1;
        }
        let name = OsString::from_vec(listed_name.to_vec());
        let relative_path = relative.join(&name);
        let path = normalizer
            .normalize(&relative_path)
            .map_err(|source| WalkError::Path {
                path: relative_path.clone(),
                source,
            })?;
        debug_assert_eq!(
            path.comparison_key().as_bytes(),
            key,
            "the key the rejections above are decided on is the key this path carries"
        );
        // A filesystem whose listing carries no entry type is stat'd to learn
        // one, which opens the same window one call earlier: a name gone by the
        // time this runs is a name this page read nothing at. It is the same
        // doctrine through the same reading, so the two windows cannot answer a
        // deleted name differently — the candidate is taken in either way, and
        // the loop below states it.
        let listed = if entry.file_type() == FileType::Unknown {
            stat_listed(directory, &name, &relative_path, Paged::default())?
                .map(|metadata| classify_file_type(FileType::from_raw_mode(metadata.st_mode as _)))
        } else {
            Some(classify_file_type(entry.file_type()))
        };
        let mut sort_key = path.comparison_key().as_bytes().to_vec();
        if listed == Some(EntryKind::Directory)
            && !exclusions.excludes(&path)
            && !normalizer.names_a_shadow(&name)
        {
            sort_key.push(b'/');
        }
        // The separator a descendable directory sorts under is the one thing
        // the key above could not carry, so the cursor is asked again with the
        // sort key in hand.
        if cursor.is_some_and(|cursor| sort_key.as_slice() <= cursor) {
            continue;
        }
        run.offer(Candidate {
            name,
            path,
            listed,
            sort_key,
        });
    }
    let mut candidates = run.ascending();
    let more = candidates.len() > FRONTIER_PAGE;
    candidates.truncate(FRONTIER_PAGE);
    let covered = candidates.last().map(|last| last.sort_key.clone());

    let mut pending = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        // The sort key is spent: the page's own run of names is what the cursor
        // carries, so nothing past this point reads one entry's key.
        let Candidate {
            name, path, listed, ..
        } = candidate;
        let relative_path = relative.join(&name);
        let observed = observe_listed(
            root_fd,
            directory,
            &name,
            &relative_path,
            &path,
            listed,
            normalizer,
            exclusions,
            faults,
        )?;
        pending.push(Pending {
            parent: directory.clone(),
            name,
            path,
            observed,
        });
    }
    Ok(Page {
        pending,
        more,
        covered,
        #[cfg(test)]
        identified,
    })
}

/// What the page states for one name the listing named.
///
/// The three convergences at this window are all here, and they are one answer:
/// a name already gone when the listing's own stat ran, a name gone when the
/// page's stat runs, and a name holding a kind the listing did not name. The
/// third is the second seen a moment later — the name was unlinked and something
/// took it — so all three say the page read nothing at the name. A symbolic link
/// whose target read meets the same absence is the fourth reading of it, one
/// call further on.
///
/// Everything else is the entry the listing named, classified.
#[allow(clippy::too_many_arguments)] // The whole of one entry's reading, in one place.
fn observe_listed(
    root_fd: &Arc<OwnedFd>,
    directory: &Arc<OwnedFd>,
    name: &OsStr,
    access: &Path,
    path: &NormalizedPath,
    listed: Option<EntryKind>,
    normalizer: &PathNormalizer,
    exclusions: &Exclusions,
    faults: &WalkFaults,
) -> Result<Observed, WalkError> {
    let Some(kind) = listed else {
        return Ok(Observed::Skipped(SkipReason::Vanished));
    };
    let armed = faults.paging(kind);
    let Some(metadata) = stat_listed(directory, name, access, armed)? else {
        return Ok(Observed::Skipped(SkipReason::Vanished));
    };
    let observed_kind = armed
        .observes
        .unwrap_or_else(|| classify_file_type(FileType::from_raw_mode(metadata.st_mode as _)));
    if observed_kind != kind {
        return Ok(Observed::Skipped(SkipReason::Vanished));
    }
    if let Some(reason) = exclusions.reason(path) {
        return Ok(Observed::Skipped(reason.into()));
    }
    if normalizer.names_a_shadow(name) {
        return Ok(Observed::Skipped(SkipReason::Shadow));
    }
    Ok(match kind {
        EntryKind::Symlink => {
            match classify_link(root_fd, path.as_path(), directory, name, normalizer)? {
                Some(link) => Observed::Skipped(SkipReason::SymbolicLink(link)),
                None => Observed::Skipped(SkipReason::Vanished),
            }
        }
        EntryKind::Special(special) => Observed::Skipped(SkipReason::SpecialFile(special)),
        EntryKind::Directory => Observed::Directory,
        EntryKind::File => Observed::File(stat_raw(&metadata)),
    })
}

/// One stat of a name a listing named, taken inside the window that listing
/// opened.
///
/// **Absence is an answer here, never a refusal**, and `armed` is what lets a
/// case reach that answer: the seam hands back the error number the stat meets
/// in place of running, and the reading below is the production one either way.
/// Every other error number is the machine failing to answer, which ends the
/// walk.
#[allow(clippy::disallowed_methods)] // norn-fs owns the vault walk and stat.
fn stat_listed(
    directory: &Arc<OwnedFd>,
    name: &OsStr,
    access: &Path,
    armed: Paged,
) -> Result<Option<rustix::fs::Stat>, WalkError> {
    crate::reads::count_stat();
    let observed = match armed.meets {
        Some(errno) => Err(errno),
        None => statat(directory, name, AtFlags::SYMLINK_NOFOLLOW),
    };
    match observed {
        Ok(metadata) => Ok(Some(metadata)),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(source) => Err(environment_errno("stating", access, source)),
    }
}

/// What the link at `name` names, or nothing where the name no longer holds a
/// link.
///
/// **The stat that named this a link and this read of it are two observations.**
/// A writer can unlink the link between them, or replace it with a name that is
/// not a link at all — the reads answer `ENOENT`, `ENOTDIR` and `EINVAL` — and
/// all three say the same thing the rest of the walk's windows say: nothing here
/// read the name. Every other error number is the machine failing to answer and
/// refuses.
#[allow(clippy::disallowed_methods)] // norn-fs owns vault links and stat.
fn classify_link(
    root_fd: &Arc<OwnedFd>,
    relative_link: &Path,
    parent: &Arc<OwnedFd>,
    name: &OsStr,
    normalizer: &PathNormalizer,
) -> Result<Option<LinkKind>, WalkError> {
    let target = match readlinkat(parent, name, Vec::new()) {
        Ok(target) => target,
        Err(rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR | rustix::io::Errno::INVAL) => {
            return Ok(None);
        }
        Err(source) => return Err(environment_errno("reading link", relative_link, source)),
    };
    let target = PathBuf::from(OsString::from_vec(target.into_bytes()));
    if target.is_absolute() {
        return Ok(Some(LinkKind::Outbound));
    }
    let Some(relative) = resolve_relative_target(relative_link, &target) else {
        return Ok(Some(LinkKind::Outbound));
    };
    if relative.as_os_str().is_empty() {
        return Ok(Some(LinkKind::InVaultDirectory));
    }
    let relative = normalizer
        .normalize(&relative)
        .expect("the lexical resolver returns a non-empty relative path without parents");
    inspect_link_target(root_fd, relative.as_path())
        .map(Some)
        .map_err(|source| environment_errno("stating link target for", relative_link, source))
}

/// Resolves a link target lexically against the link's vault-relative parent.
/// Crossing above the vault root is outbound; no filesystem name is followed.
fn resolve_relative_target(link: &Path, target: &Path) -> Option<PathBuf> {
    use std::path::Component;

    let mut parts: Vec<_> = link
        .parent()?
        .components()
        .map(|part| part.as_os_str().to_owned())
        .collect();
    for component in target.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => parts.push(part.to_owned()),
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(parts.into_iter().collect())
}

fn classify_file_type(kind: FileType) -> EntryKind {
    if kind == FileType::Directory {
        EntryKind::Directory
    } else if kind == FileType::RegularFile {
        EntryKind::File
    } else if kind == FileType::Symlink {
        EntryKind::Symlink
    } else if kind == FileType::Fifo {
        EntryKind::Special(FileKind::Fifo)
    } else if kind == FileType::Socket {
        EntryKind::Special(FileKind::Socket)
    } else if kind == FileType::BlockDevice {
        EntryKind::Special(FileKind::BlockDevice)
    } else if kind == FileType::CharacterDevice {
        EntryKind::Special(FileKind::CharacterDevice)
    } else {
        EntryKind::Special(FileKind::Other)
    }
}

fn inspect_link_target(
    root: &Arc<OwnedFd>,
    relative: &Path,
) -> Result<LinkKind, rustix::io::Errno> {
    let mut directory = root.clone();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let name = component.as_os_str();
        if components.peek().is_some() {
            match openat(&directory, name, directory_flags(), Mode::empty()) {
                Ok(fd) => directory = Arc::new(fd),
                Err(
                    rustix::io::Errno::NOENT | rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR,
                ) => return Ok(LinkKind::Dangling),
                Err(error) => return Err(error),
            }
        } else {
            crate::reads::count_stat();
            return match statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => match FileType::from_raw_mode(stat.st_mode as _) {
                    FileType::Directory => Ok(LinkKind::InVaultDirectory),
                    FileType::Symlink => Ok(LinkKind::Dangling),
                    _ => Ok(LinkKind::InVaultFile),
                },
                Err(
                    rustix::io::Errno::NOENT | rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR,
                ) => Ok(LinkKind::Dangling),
                Err(error) => Err(error),
            };
        }
    }
    Ok(LinkKind::InVaultDirectory)
}

fn stat(metadata: &fs::Metadata) -> FileStat {
    FileStat {
        len: metadata.len(),
        mtime: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        identity: identity_of(metadata),
    }
}

#[allow(clippy::unnecessary_cast)] // st_dev's native width differs between supported Unix targets.
fn stat_raw(metadata: &rustix::fs::Stat) -> FileStat {
    let mtime = if metadata.st_mtime >= 0 {
        SystemTime::UNIX_EPOCH
            + std::time::Duration::new(metadata.st_mtime as u64, metadata.st_mtime_nsec as u32)
    } else {
        SystemTime::UNIX_EPOCH
    };
    FileStat {
        len: metadata.st_size as u64,
        mtime,
        identity: Identity {
            dev: metadata.st_dev as u64,
            ino: metadata.st_ino,
        },
    }
}

fn environment_errno(operation: &'static str, path: &Path, source: rustix::io::Errno) -> WalkError {
    environment(
        operation,
        path,
        io::Error::from_raw_os_error(source.raw_os_error()),
    )
}

fn environment(operation: &'static str, path: &Path, source: io::Error) -> WalkError {
    WalkError::Environment {
        operation,
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::faults::{Answer, Stage};
    use super::*;
    use crate::path::CaseSensitivity;
    use crate::scratch::Scratch;
    use std::io::{Cursor, Read};

    /// A walk of `root` whose paging window holds the foreign edit `armed`
    /// names. The arm is read while a page is stat'd, which is after
    /// construction, so a walk armed here meets it at its first page.
    fn walk_armed(root: &Path, armed: &'static [(Stage, Answer)]) -> Walk {
        let mut walk = walk(root, &[]).expect("walk");
        walk.faults = WalkFaults::at(armed);
        walk
    }

    fn paths(walk: Walk) -> Vec<(PathBuf, Option<SkipReason>)> {
        walk.map(|fact| match fact.expect("walk fact") {
            WalkFact::File(file) => (file.path().as_path().to_owned(), None),
            WalkFact::Skipped(skip) => (skip.path().as_path().to_owned(), Some(skip.reason())),
        })
        .collect()
    }

    #[test]
    fn walks_dot_content_and_non_markdown_files_in_lexicographic_order() {
        let scratch = Scratch::new("walk-order");
        scratch.place("z.md", b"z");
        scratch.place(".hidden.md", b"hidden");
        scratch.directory("vault/.obsidian");
        scratch.place(".obsidian/state.json", b"{}");
        scratch.directory("vault/a");
        scratch.place("a/x.bin", &[0xff]);
        scratch.place("a.md", b"a");

        let facts = paths(walk(&scratch.at(""), &[]).expect("walk"));
        assert_eq!(
            facts,
            [
                ".hidden.md",
                ".obsidian/state.json",
                "a.md",
                "a/x.bin",
                "z.md"
            ]
            .into_iter()
            .map(|path| (PathBuf::from(path), None))
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn exclusions_mechanism_and_shadows_are_one_root_fact_each() {
        let scratch = Scratch::new("walk-skips");
        scratch.directory("vault/excluded/deep");
        scratch.place("excluded/deep/no.md", b"no");
        scratch.directory("vault/.norn/tmp/deep");
        scratch.place(".norn/tmp/deep/no.md", b"no");
        scratch.place("norn-shadow-7-2", b"no");
        scratch.place("norn-shadow-notes.md", b"yes");

        let facts = paths(walk(&scratch.at(""), &[PathBuf::from("./excluded")]).expect("walk"));
        assert_eq!(
            facts,
            vec![
                (PathBuf::from(".norn/tmp"), Some(SkipReason::Mechanism)),
                (PathBuf::from("excluded"), Some(SkipReason::HostExclusion)),
                (PathBuf::from("norn-shadow-7-2"), Some(SkipReason::Shadow)),
                (PathBuf::from("norn-shadow-notes.md"), None),
            ]
        );
    }

    #[test]
    fn reading_returns_bytes_and_the_hash_of_those_same_bytes() {
        let scratch = Scratch::new("walk-read");
        scratch.place("note.md", b"one observation");
        let fact = walk(&scratch.at(""), &[])
            .expect("walk")
            .next()
            .expect("one fact")
            .expect("file fact");
        let WalkFact::File(file) = fact else {
            panic!("file was skipped")
        };
        let read = file
            .read_optional()
            .expect("read")
            .expect("the file the walk just yielded");
        assert_eq!(read.bytes(), b"one observation");
        assert_eq!(read.content_hash(), ContentHash::of(read.bytes()));
        assert_eq!(read.stat().len, read.bytes().len() as u64);
    }

    #[test]
    fn hashing_and_returning_bytes_is_one_forward_read_pass() {
        struct Counted {
            inner: Cursor<Vec<u8>>,
            bytes: usize,
        }

        impl Read for Counted {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let read = self.inner.read(buffer)?;
                self.bytes += read;
                Ok(read)
            }
        }

        let mut reader = Counted {
            inner: Cursor::new(b"one pass only".to_vec()),
            bytes: 0,
        };
        let (bytes, hash) = read_bytes_and_hash(&mut reader).expect("one reading");
        assert_eq!(reader.bytes, bytes.len());
        assert_eq!(hash, ContentHash::of(&bytes));
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging unsupported links.
    fn symbolic_links_are_noted_by_species_and_never_traversed() {
        let scratch = Scratch::new("walk-links");
        scratch.place("target.md", b"inside file");
        scratch.directory("vault/dir");
        scratch.place("dir/child.md", b"inside directory");
        let outside = scratch.path("outside.md");
        fs::write(&outside, b"outside bytes").expect("outside canary");
        std::os::unix::fs::symlink("target.md", scratch.at("file-link.md")).expect("file link");
        std::os::unix::fs::symlink("dir", scratch.at("dir-link")).expect("directory link");
        std::os::unix::fs::symlink("missing.md", scratch.at("dangling.md")).expect("dangling link");
        std::os::unix::fs::symlink("../outside.md", scratch.at("outbound.md"))
            .expect("outbound link");

        let facts = paths(walk(&scratch.at(""), &[]).expect("walk"));
        for (path, kind) in [
            ("dangling.md", LinkKind::Dangling),
            ("dir-link", LinkKind::InVaultDirectory),
            ("file-link.md", LinkKind::InVaultFile),
            ("outbound.md", LinkKind::Outbound),
        ] {
            assert!(facts.contains(&(PathBuf::from(path), Some(SkipReason::SymbolicLink(kind)))));
        }
        assert_eq!(
            facts
                .iter()
                .filter(|(path, _)| path == Path::new("dir/child.md"))
                .count(),
            1,
            "the directory link was followed and duplicated its child"
        );
    }

    /// **A link standing where the page listed a directory is never traversed.**
    /// The open refuses to follow it, so the name is one the walk read nothing at
    /// — the kind change any other replacement in this window is — and nothing
    /// outside the vault is reached through it. Containment holds because the
    /// descent never follows a name it did not open as a directory, not because
    /// the walk stops.
    #[test]
    #[allow(clippy::disallowed_methods)]
    fn replacing_a_pending_directory_with_an_outbound_link_cannot_escape() {
        let scratch = Scratch::new("walk-directory-swap");
        scratch.place("a.md", b"first");
        scratch.directory("vault/z-pending");
        scratch.place("z-pending/inside.md", b"inside");
        scratch.directory("outside");
        fs::write(scratch.path("outside/canary.md"), b"outside").expect("outside canary");

        let mut facts = walk(&scratch.at(""), &[]).expect("walk");
        assert_eq!(
            facts.next().expect("first").expect("fact").path().as_path(),
            Path::new("a.md")
        );
        fs::remove_dir_all(scratch.at("z-pending")).expect("remove pending directory");
        std::os::unix::fs::symlink("../../outside", scratch.at("z-pending"))
            .expect("replacement link");

        assert_eq!(
            paths(facts),
            vec![(PathBuf::from("z-pending"), Some(SkipReason::Vanished))],
            "the replacement was followed or the walk concluded something about it"
        );
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn a_link_chain_is_classified_without_following_the_outbound_second_link() {
        let scratch = Scratch::new("walk-link-chain");
        fs::write(scratch.path("outside.md"), b"outside").expect("outside canary");
        std::os::unix::fs::symlink("../outside.md", scratch.at("second.md"))
            .expect("outbound second link");
        std::os::unix::fs::symlink("second.md", scratch.at("first.md")).expect("first link");

        let facts = paths(walk(&scratch.at(""), &[]).expect("walk"));
        assert!(facts.contains(&(
            PathBuf::from("first.md"),
            Some(SkipReason::SymbolicLink(LinkKind::Dangling))
        )));
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn links_resolving_to_the_vault_root_are_in_vault_directories() {
        let scratch = Scratch::new("walk-link-root");
        scratch.directory("vault/dir");
        std::os::unix::fs::symlink(".", scratch.at("root-link")).expect("dot link");
        std::os::unix::fs::symlink("..", scratch.at("dir/parent-link")).expect("parent link");
        let facts = paths(walk(&scratch.at(""), &[]).expect("walk"));
        for path in ["root-link", "dir/parent-link"] {
            assert!(facts.contains(&(
                PathBuf::from(path),
                Some(SkipReason::SymbolicLink(LinkKind::InVaultDirectory))
            )));
        }
    }

    #[test]
    fn special_files_are_typed_skips_and_are_never_opened_for_content() {
        let scratch = Scratch::new("walk-fifo");
        let status = std::process::Command::new("mkfifo")
            .arg(scratch.at("pipe"))
            .status()
            .expect("run mkfifo");
        assert!(status.success(), "mkfifo failed");
        let facts = paths(walk(&scratch.at(""), &[]).expect("walk"));
        assert_eq!(
            facts,
            vec![(
                PathBuf::from("pipe"),
                Some(SkipReason::SpecialFile(FileKind::Fifo))
            )]
        );
    }

    /// **The window between enumeration and open is ordinary churn.** A name
    /// the walk yielded as a file and a replacement written before the open are
    /// two observations of a tree other writers are editing, and the second one
    /// is the current truth: no file is there to read.
    ///
    /// The forbidden shape is a refusal, which would report a foreign edit as a
    /// broken environment and hold the read open against a pipe nobody writes
    /// to.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: replacing a deferred name.
    fn a_deferred_file_replaced_by_a_fifo_answers_absence_without_blocking() {
        let scratch = Scratch::new("walk-file-fifo-swap");
        scratch.place("note.md", b"first");
        let fact = walk(&scratch.at(""), &[])
            .expect("walk")
            .next()
            .expect("one fact")
            .expect("file fact");
        let WalkFact::File(file) = fact else {
            panic!("file was skipped")
        };
        fs::remove_file(scratch.at("note.md")).expect("remove original");
        let status = std::process::Command::new("mkfifo")
            .arg(scratch.at("note.md"))
            .status()
            .expect("run mkfifo");
        assert!(status.success(), "mkfifo failed");

        assert!(
            file.read_optional()
                .expect("a replaced name is not a machine failure")
                .is_none(),
            "a fifo at a deferred name read as document content"
        );
    }

    /// The same window, closed by a deletion rather than by a replacement: the
    /// file the walk enumerated is gone before the open reaches it.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: removing a deferred name.
    fn a_deferred_file_deleted_before_its_open_answers_absence() {
        let scratch = Scratch::new("walk-file-delete-race");
        scratch.place("note.md", b"first");
        let fact = walk(&scratch.at(""), &[])
            .expect("walk")
            .next()
            .expect("one fact")
            .expect("file fact");
        let WalkFact::File(file) = fact else {
            panic!("file was skipped")
        };
        fs::remove_file(scratch.at("note.md")).expect("remove the enumerated file");

        assert!(
            file.read_optional()
                .expect("a deleted name is not a machine failure")
                .is_none(),
            "a deleted document read as content"
        );
    }

    /// The same window, closed by a directory taking the name. A walk begun now
    /// descends into it and yields no file there, which is what the absence
    /// converges on.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: replacing a deferred name.
    fn a_deferred_file_replaced_by_a_directory_answers_absence() {
        let scratch = Scratch::new("walk-file-directory-race");
        scratch.place("note.md", b"first");
        let fact = walk(&scratch.at(""), &[])
            .expect("walk")
            .next()
            .expect("one fact")
            .expect("file fact");
        let WalkFact::File(file) = fact else {
            panic!("file was skipped")
        };
        fs::remove_file(scratch.at("note.md")).expect("remove original");
        fs::create_dir(scratch.at("note.md")).expect("a directory takes the name");

        assert!(
            file.read_optional()
                .expect("a replaced name is not a machine failure")
                .is_none(),
            "a directory at a deferred name read as document content"
        );
    }

    /// **The paging window converges on absence.** A name the listing returned
    /// and a deletion that lands before its stat are two observations of a tree
    /// other writers are editing, and the second one is the current truth: there
    /// is no entry there. The walk states the name as one it read nothing at and
    /// keeps going.
    ///
    /// The forbidden shape is a refusal, which reports one foreign deletion as a
    /// broken environment and takes the whole enumeration — every sibling
    /// included — down with it. **The other forbidden shape is silence**: a name
    /// passed over with no notation is one a consumer that prunes by enumeration
    /// reads as a name this walk read and found nothing under.
    #[test]
    fn an_entry_deleted_between_the_listing_and_its_stat_leaves_the_page() {
        let scratch = Scratch::new("walk-page-vanishes");
        for name in ["a.md", "b.md", "c.md"] {
            scratch.place(name, b"x");
        }

        let facts = paths(walk_armed(
            &scratch.at(""),
            &[(Stage::Page, Answer::Vanishes)],
        ));
        assert_eq!(
            facts,
            vec![
                (PathBuf::from("a.md"), Some(SkipReason::Vanished)),
                (PathBuf::from("b.md"), None),
                (PathBuf::from("c.md"), None)
            ]
        );
    }

    /// **A kind change in the same window is the same vanishing.** The name the
    /// listing named was unlinked and something else took it, so what stands
    /// there is not the entry that was listed — and the walk states it exactly as
    /// it states a name that is simply gone.
    #[test]
    fn an_entry_whose_kind_changed_in_the_window_leaves_the_page_too() {
        let scratch = Scratch::new("walk-page-replaced");
        for name in ["a.md", "b.md", "c.md"] {
            scratch.place(name, b"x");
        }

        let facts = paths(walk_armed(
            &scratch.at(""),
            &[(Stage::Page, Answer::Replaced)],
        ));
        assert_eq!(
            facts,
            vec![
                (PathBuf::from("a.md"), Some(SkipReason::Vanished)),
                (PathBuf::from("b.md"), None),
                (PathBuf::from("c.md"), None)
            ]
        );
    }

    /// **A directory that leaves a page is a root the walk never entered**, and
    /// it says so. The entries beside it are read as usual — the page's other
    /// stats are the machine's own — and nothing under the root is yielded,
    /// because there is nothing there to yield.
    ///
    /// This is the half of the convergence with a subtree behind it. A consumer
    /// concluding from this enumeration can prune what the vault no longer holds
    /// under that name and must still hold back everything it decides from
    /// *reading* the place, because this walk read none of it.
    #[test]
    fn a_directory_that_leaves_a_page_is_a_root_the_walk_read_nothing_under() {
        let scratch = Scratch::new("walk-page-vanishing-directory");
        scratch.place("a.md", b"x");
        scratch.directory("vault/sub");
        scratch.place("sub/x.md", b"x");
        scratch.place("sub/y.md", b"y");
        scratch.place("z.md", b"z");

        let mut walk = walk(&scratch.at(""), &[]).expect("walk");
        walk.faults = WalkFaults::at(&[(Stage::Page, Answer::Vanishes)])
            .over(super::faults::Reach::Directory);
        assert_eq!(
            paths(walk),
            vec![
                (PathBuf::from("a.md"), None),
                (PathBuf::from("sub"), Some(SkipReason::Vanished)),
                (PathBuf::from("z.md"), None),
            ]
        );
    }

    /// **A directory the page listed and something else replaced is the same
    /// root.** The name still holds a thing, and it is not the thing the listing
    /// named, so the walk read nothing at the name and nothing under it.
    #[test]
    fn a_directory_whose_kind_changed_in_the_window_is_the_same_root() {
        let scratch = Scratch::new("walk-page-replaced-directory");
        scratch.place("a.md", b"x");
        scratch.directory("vault/sub");
        scratch.place("sub/x.md", b"x");

        let mut walk = walk(&scratch.at(""), &[]).expect("walk");
        walk.faults = WalkFaults::at(&[(Stage::Page, Answer::Replaced)])
            .over(super::faults::Reach::Directory);
        assert_eq!(
            paths(walk),
            vec![
                (PathBuf::from("a.md"), None),
                (PathBuf::from("sub"), Some(SkipReason::Vanished)),
            ]
        );
    }

    /// **The widest of the walk's windows converges the same way.** A page is
    /// stat'd whole and hands its entries out one at a time, so a directory it
    /// listed is opened only after every earlier entry has been consumed — the
    /// span between that stat and that open is the whole of the reading in
    /// between, not one call. A writer removing the directory in it leaves
    /// nothing to enter, which is the answer a walk begun now gives.
    ///
    /// No seam arranges this one: the window is wide enough to reach from a test.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: removing a listed directory.
    fn a_directory_removed_before_its_own_open_is_a_root_the_walk_read_nothing_under() {
        let scratch = Scratch::new("walk-descent-vanishes");
        scratch.place("a.md", b"x");
        scratch.directory("vault/z-dir");
        scratch.place("z-dir/inside.md", b"inside");

        let mut walk = walk(&scratch.at(""), &[]).expect("walk");
        let first = walk.next().expect("the first fact").expect("a file fact");
        assert_eq!(first.path().as_path(), Path::new("a.md"));
        fs::remove_dir_all(scratch.at("z-dir")).expect("remove the listed directory");

        assert_eq!(
            paths(walk),
            vec![(PathBuf::from("z-dir"), Some(SkipReason::Vanished))]
        );
    }

    /// The same window, closed by something that is not a directory taking the
    /// name. Nothing there is the entry the page listed, so the walk enters
    /// nothing and states the root it did not enter.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: replacing a listed directory.
    fn a_directory_replaced_before_its_own_open_is_the_same_root() {
        let scratch = Scratch::new("walk-descent-replaced");
        scratch.place("a.md", b"x");
        scratch.directory("vault/z-dir");
        scratch.place("z-dir/inside.md", b"inside");

        let mut walk = walk(&scratch.at(""), &[]).expect("walk");
        walk.next().expect("the first fact").expect("a file fact");
        fs::remove_dir_all(scratch.at("z-dir")).expect("remove the listed directory");
        fs::write(scratch.at("z-dir"), b"a file now").expect("a file takes the name");

        assert_eq!(
            paths(walk),
            vec![(PathBuf::from("z-dir"), Some(SkipReason::Vanished))]
        );
    }

    /// **A machine that will not answer at the descent still refuses.** A
    /// directory whose open is denied says nothing about whether it is there, so
    /// the walk ends rather than reporting a root it read nothing under.
    #[test]
    fn a_directory_the_machine_will_not_open_refuses_the_walk() {
        let scratch = Scratch::new("walk-descent-denied");
        scratch.place("a.md", b"x");
        let folder = scratch.directory("vault/z-dir");
        scratch.place("z-dir/inside.md", b"inside");

        let mut walk = walk(&scratch.at(""), &[]).expect("walk");
        walk.next().expect("the first fact").expect("a file fact");
        scratch.set_mode(&folder, 0o000);

        let outcome = walk.next().expect("the directory's position");
        scratch.set_mode(&folder, 0o755);
        let Err(error) = outcome else {
            panic!("a directory the machine would not open read as a root that left")
        };
        assert!(error.to_string().contains("z-dir"), "{error}");
    }

    /// **A link read after the name stopped holding one converges too.** The
    /// stat that named the entry a symbolic link and the read of that link are
    /// two observations, and a writer between them leaves either no name at all
    /// or a name that is not a link — `ENOENT` and `EINVAL`. Both say the walk
    /// read nothing there.
    #[test]
    fn a_link_that_stopped_being_one_is_a_name_the_walk_read_nothing_at() {
        let scratch = Scratch::new("walk-link-window");
        scratch.place("ordinary.md", b"not a link");
        let root = scratch.at("");
        let normalizer = PathNormalizer::detect(&root).expect("normalizer");
        let root_fd = Arc::new(open(&root, directory_flags(), Mode::empty()).expect("root fd"));

        for name in ["ordinary.md", "never-existed.md"] {
            let observed = classify_link(
                &root_fd,
                Path::new(name),
                &root_fd,
                OsStr::new(name),
                &normalizer,
            )
            .expect("a name that holds no link is not a machine failure");
            assert_eq!(observed, None, "{name} read as a link");
        }
    }

    /// **A machine that will not answer still refuses, at this window too.** A
    /// denied stat says nothing about whether an entry is there, and converging
    /// on absence would let a revoked permission prune every row under the
    /// directory. Only absence converges; every other error number ends the
    /// walk.
    #[test]
    fn an_entry_the_machine_will_not_stat_refuses_the_walk() {
        let scratch = Scratch::new("walk-page-denied");
        for name in ["a.md", "b.md", "c.md"] {
            scratch.place(name, b"x");
        }

        let mut facts = walk_armed(&scratch.at(""), &[(Stage::Page, Answer::Denied)]);
        let error = facts
            .next()
            .expect("the page's position")
            .expect_err("a stat the machine refused");
        assert!(error.to_string().contains("a.md"), "{error}");
        assert!(facts.next().is_none(), "a walk error must be terminal");
    }

    /// **A page holds one entry per name it covered, so the run it covered and
    /// the entries it hands out are one list.** The next page of the same
    /// directory starts after the last name this one listed, and a name that
    /// went away inside the page's own window is one of those names — so it
    /// neither hides the names between it and the page boundary nor yields the
    /// page's own tail twice.
    #[test]
    fn a_page_holding_a_vanished_name_pages_the_rest_of_the_directory_once() {
        let scratch = Scratch::new("walk-page-vanishes-wide");
        for index in 0..(FRONTIER_PAGE + 1) {
            scratch.place(&format!("{index:04}.md"), b"x");
        }

        let facts = paths(walk_armed(
            &scratch.at(""),
            &[(Stage::Page, Answer::Vanishes)],
        ));
        let mut expected: Vec<(PathBuf, Option<SkipReason>)> =
            vec![(PathBuf::from("0000.md"), Some(SkipReason::Vanished))];
        expected.extend(
            (1..(FRONTIER_PAGE + 1)).map(|index| (PathBuf::from(format!("{index:04}.md")), None)),
        );
        assert_eq!(facts, expected);
    }

    /// **A subtree walk answers a root that left the same way.** The caller
    /// reached this scope from an observation of its own, and a writer between
    /// that observation and this descent is the vault evolving: the walk states
    /// the root it read nothing under and yields nothing else.
    #[test]
    fn a_subtree_whose_root_is_gone_is_a_root_the_walk_read_nothing_under() {
        let scratch = Scratch::new("walk-subtree-vanished");
        scratch.place("keep.md", b"keep");
        scratch.place("file.md", b"a file, not a directory");

        assert_eq!(
            paths(walk_subtree(&scratch.at(""), Path::new("gone"), &[]).expect("walk")),
            vec![(PathBuf::from("gone"), Some(SkipReason::Vanished))]
        );
        assert_eq!(
            paths(walk_subtree(&scratch.at(""), Path::new("file.md/under"), &[]).expect("walk")),
            vec![(PathBuf::from("file.md"), Some(SkipReason::Vanished))],
            "a component that is not a directory read as a machine failure"
        );
    }

    #[test]
    fn retained_file_facts_share_one_root_descriptor() {
        let scratch = Scratch::new("walk-fact-descriptors");
        for index in 0..300 {
            scratch.directory(&format!("vault/{index:04}"));
            scratch.place(&format!("{index:04}/note.md"), b"x");
        }
        let files = walk(&scratch.at(""), &[])
            .expect("walk")
            .map(|fact| match fact.expect("fact") {
                WalkFact::File(file) => file,
                WalkFact::Skipped(_) => panic!("fixture contains no skips"),
            })
            .collect::<Vec<_>>();
        assert_eq!(files.len(), 300);
        assert!(
            files
                .iter()
                .all(|file| Arc::ptr_eq(&files[0].root_fd, &file.root_fd))
        );
    }

    /// **A directory sorts under a separator its own name does not carry, and
    /// the page boundary is where that matters.**
    ///
    /// `note` the directory sorts after `note.md` the file, because the walk
    /// yields it where its children fall. So a page that ends at `note.md`
    /// leaves a cursor that `note`'s own name sorts at or below while the entry
    /// it stands for sorts above — and a page deciding on the name alone would
    /// drop the whole subtree. The fixture puts exactly that boundary one page
    /// in.
    #[test]
    fn a_directory_named_inside_the_page_boundary_is_paged_after_the_file_it_shares_a_stem_with() {
        let scratch = Scratch::new("walk-page-boundary-stem");
        for index in 0..(FRONTIER_PAGE - 1) {
            scratch.place(&format!("a{index:04}.md"), b"x");
        }
        scratch.place("note.md", b"the page's last entry");
        scratch.directory("vault/note");
        scratch.place("note/inner.md", b"under the directory that shares the stem");
        scratch.place("zz.md", b"past the boundary");

        let mut expected: Vec<(PathBuf, Option<SkipReason>)> = (0..(FRONTIER_PAGE - 1))
            .map(|index| (PathBuf::from(format!("a{index:04}.md")), None))
            .collect();
        expected.push((PathBuf::from("note.md"), None));
        expected.push((PathBuf::from("note/inner.md"), None));
        expected.push((PathBuf::from("zz.md"), None));
        assert_eq!(paths(walk(&scratch.at(""), &[]).expect("walk")), expected);
    }

    /// **A page stops identifying a name its run is already full past, and this
    /// is where that rejection is held.**
    ///
    /// What the rejection saves is a function of the order a filesystem returns
    /// names in — everything past one page's worth where the stream runs low
    /// keys first, nothing at all where it runs them last — so no bar over a
    /// walk's own counts can gate it, and the identity bar below states that it
    /// does not. The decision carries no such order, so it is asked here
    /// directly: a run holding a full page and its lookahead at lower keys takes
    /// no name above them, a run with room takes any name, and the cursor
    /// rejects underneath both.
    #[test]
    fn a_page_stops_identifying_a_name_its_run_is_already_full_past() {
        let paths = PathNormalizer::for_sensitivity(CaseSensitivity::Sensitive);
        let candidate = |name: &str| {
            let path = paths
                .normalize(Path::new(name))
                .expect("a vault-relative name");
            let sort_key = path.comparison_key().as_bytes().to_vec();
            Candidate {
                name: OsString::from(name),
                path,
                listed: Some(EntryKind::File),
                sort_key,
            }
        };

        let mut run = Run::new();
        for index in 0..Run::CAPACITY {
            let name = format!("{index:05}.md");
            assert!(
                worth_identifying(&run, None, name.as_bytes()),
                "a run with room refused {name}"
            );
            run.offer(candidate(&name));
        }

        assert!(
            !worth_identifying(&run, None, b"99999.md"),
            "a full run identified a name above every key it holds"
        );
        assert!(
            worth_identifying(&run, None, b"00100.md"),
            "a full run refused a name below the key it would drop"
        );
        assert!(
            !worth_identifying(&run, Some(b"00100.md"), b"00099.md"),
            "a name an earlier page handed out was identified again"
        );
        assert!(
            worth_identifying(&run, Some(b"00100.md"), b"00100.md"),
            "the cursor's own name is the one a key alone cannot settle"
        );
    }

    /// **A name replaced in place between two pages is reported at both kinds:
    /// one walk yields the document and then the tree that took its name.**
    ///
    /// The cursor a page leaves is a sort key, and a directory sorts under a
    /// separator its own name does not carry — so the names a later page cannot
    /// reject on the key alone are the ones the cursor runs into, its own
    /// included. A document handed out under that name, standing as a directory
    /// when the next page lists it, is listed a second time and sorts above the
    /// cursor this time. Both readings are things this walk read, so the pair
    /// stands rather than one of them pruning the other.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: replacing a paged name.
    fn a_name_replaced_between_two_pages_is_reported_as_both_a_document_and_a_directory() {
        let scratch = Scratch::new("walk-page-boundary-replacement");
        for index in 0..FRONTIER_PAGE {
            scratch.place(&format!("{index:05}.md"), b"x");
        }
        scratch.place("zzzzz.md", b"past the first page");
        let boundary = format!("{:05}.md", FRONTIER_PAGE - 1);

        let mut walk = walk(&scratch.at(""), &[]).expect("walk");
        let first_page: Vec<PathBuf> = walk
            .by_ref()
            .take(FRONTIER_PAGE)
            .map(|fact| match fact.expect("walk fact") {
                WalkFact::File(file) => file.path().as_path().to_owned(),
                WalkFact::Skipped(skip) => {
                    panic!("unexpected skip at {}", skip.path().as_path().display())
                }
            })
            .collect();
        assert_eq!(
            first_page.last().map(PathBuf::as_path),
            Some(Path::new(&boundary)),
            "the fixture's first page does not end at the name the case replaces"
        );

        fs::remove_file(scratch.at(&boundary)).expect("remove the paged document");
        scratch.directory(&format!("vault/{boundary}"));
        scratch.place(
            &format!("{boundary}/inner.md"),
            b"under the name the page handed out",
        );

        let rest: Vec<(PathBuf, Option<SkipReason>)> = paths(walk);
        assert_eq!(
            rest,
            vec![
                (PathBuf::from(format!("{boundary}/inner.md")), None),
                (PathBuf::from("zzzzz.md"), None),
            ],
            "the replaced name's new tree is not walked after the document at it"
        );
    }

    /// The width the two paging bars below are stated at, and the width the
    /// paging shape is accepted at.
    ///
    /// Ten thousand entries in one directory is past what a vault of notes
    /// usually holds and inside what an attachment volume, a clipping inbox or
    /// a generated export reaches, so it is the width where the paging shape's
    /// cost is worth an authored number rather than an estimate.
    ///
    /// **It is also where the acceptance stops.** The listing cost is
    /// `width * ceil(width / FRONTIER_PAGE)`, so ten times the width is a
    /// hundred times the reads, and the same shapes that reach ten thousand
    /// reach a hundred thousand. At that width one directory pages through
    /// 39,100,000 dirents and takes tens of seconds of a release build's walk,
    /// on every heal that reaches it — a number this shape is not argued for and
    /// no bar here accepts. What answers a directory that wide is the spilled
    /// sorted listing [`directory_page`] states the price of, not a wider bar.
    const WIDE_BAR_WIDTH: usize = 10_000;

    /// **The listing cost this paging shape accepts, gated.**
    ///
    /// A page must know the lowest keys the directory still holds, and a
    /// directory stream hands its names back in no order at all — so a page
    /// that retains one page's worth of names has to read every name to find
    /// them. Retaining more in memory is what would read fewer, and the
    /// frontier's memory bound is what forbids that: a walk's memory is a
    /// function of depth and the fixed page size and never of one directory's
    /// fan-out. The listing cost is therefore `width * pages`, quadratic in the
    /// width, and this is that figure at the stated width: 10,000 names read on
    /// each of `ceil(10000 / 256) = 40` pages. Nothing but the width and the
    /// page size moves it — no host, filesystem or listing order does — so the
    /// bar is the figure itself with no headroom.
    ///
    /// A spill would keep the memory bound and read each name once, so the bound
    /// is not the whole of the grounds; the rest is that it makes a read of the
    /// vault need a writable place and widens the listing-to-stat window to the
    /// whole walk of the directory, which [`directory_page`] states. That price
    /// buys nothing at this width and is what a directory an order of magnitude
    /// wider has to pay — [`WIDE_BAR_WIDTH`] says where the line is.
    ///
    /// It is spelled out rather than derived from [`FRONTIER_PAGE`], because a
    /// bar computed from the constant it is gating would move with it and gate
    /// nothing. A page size change is a change to this accepted cost, and
    /// re-authoring the number here is how it gets argued.
    const WIDE_BAR_DIRENTS: u64 = 400_000;

    /// **What a page pays a name's identity for, gated.**
    ///
    /// Reading a name off the stream is one fold and one comparison; turning it
    /// into an entry is a normalized path, the kind a type-less listing does not
    /// carry, and the sort key those decide. A page owes that only for names its
    /// cursor has not already covered, so the ceiling is the sum over pages of
    /// the run still ahead of each cursor — under half the listing count above,
    /// and a smaller share of it the wider the directory gets. That sum is
    /// `400,000 - 256 * (0 + 1 + ... + 39) = 200,320`.
    ///
    /// One name per cursor is admitted past it: the name whose key spells the
    /// cursor exactly, which is the previous page's own last entry. A key alone
    /// cannot tell that name from a directory of the same name, whose separator
    /// would sort it above — so it is identified and then dropped by the exact
    /// test. Thirty-nine of this width's forty pages carry a cursor, and the bar
    /// is `200,320 + 39`. No other name reaches that case here: the fixture's
    /// names are all one width, so no key is a proper prefix of another.
    ///
    /// The bar holds in whatever order the directory stream returns names, since
    /// both terms count names rather than orderings. It is spelled out rather
    /// than derived, for the reason the listing bar above is.
    ///
    /// It does not gate the second rejection — a name above every key the run in
    /// hand holds. That one saves whatever the listing order lets it: everything
    /// past the first page's worth where the stream runs low keys first, and
    /// nothing at all where it runs them last. A bar over an order no filesystem
    /// promises would be a bar over the filesystem, so that rejection is held
    /// where no order enters it:
    /// `a_page_stops_identifying_a_name_its_run_is_already_full_past` asks the
    /// decision directly, and removing it from the page leaves this bar's own
    /// count sitting exactly on the bar.
    const WIDE_BAR_IDENTITIES: u64 = 200_359;

    /// **A wide directory's paging costs what its bars accept.**
    ///
    /// The two numbers are the two halves of the same page: what it reads off
    /// the directory stream, and what it spends on the names that reading
    /// returns. The first is the accepted cost of a deterministic order under a
    /// fixed memory bound; the second is the work the cursor is supposed to
    /// spare, and it fails if a page identifies a name before consulting the
    /// cursor that already covered it.
    #[test]
    fn a_wide_directory_pages_inside_its_listing_and_identity_bars() {
        let scratch = Scratch::new("walk-wide-paging-cost");
        for index in 0..WIDE_BAR_WIDTH {
            scratch.place(&format!("{index:05}.md"), b"x");
        }
        let normalizer = PathNormalizer::detect(&scratch.at("")).expect("normalizer");
        let root_fd =
            Arc::new(open(scratch.at(""), directory_flags(), Mode::empty()).expect("root fd"));
        let exclusions = Exclusions::new(&normalizer, &[]).expect("no host roots");
        let mut frontier = DirectoryFrontier::new(root_fd.clone(), Path::new(""));

        let window = crate::reads::ReadWindow::open();
        let mut yielded = 0;
        while frontier
            .next_pending(&root_fd, &normalizer, &exclusions, &WalkFaults::default())
            .expect("page")
            .is_some()
        {
            yielded += 1;
        }
        let tally = window.finish();

        assert_eq!(yielded, WIDE_BAR_WIDTH);
        eprintln!(
            "wide paging at width {WIDE_BAR_WIDTH}: {} dirents (bar {WIDE_BAR_DIRENTS}), \
             {} identities (bar {WIDE_BAR_IDENTITIES}), {} stats",
            tally.walk_dirents, frontier.identified, tally.stats,
        );
        assert!(
            tally.walk_dirents <= WIDE_BAR_DIRENTS,
            "paging {WIDE_BAR_WIDTH} entries read {} directory entries, past the \
             {WIDE_BAR_DIRENTS} this shape accepts",
            tally.walk_dirents,
        );
        assert!(
            frontier.identified <= WIDE_BAR_IDENTITIES,
            "paging {WIDE_BAR_WIDTH} entries identified {} names, past the \
             {WIDE_BAR_IDENTITIES} the cursor leaves",
            frontier.identified,
        );
    }

    #[test]
    fn a_wide_directory_never_retains_more_than_one_frontier_page() {
        let scratch = Scratch::new("walk-wide-frontier");
        for index in 0..(FRONTIER_PAGE * 3 + 17) {
            scratch.place(&format!("{index:04}.md"), b"x");
        }
        let normalizer = PathNormalizer::detect(&scratch.at("")).expect("normalizer");
        let root_fd =
            Arc::new(open(scratch.at(""), directory_flags(), Mode::empty()).expect("root fd"));
        let exclusions = Exclusions::new(&normalizer, &[]).expect("no host roots");
        let mut frontier = DirectoryFrontier::new(root_fd.clone(), Path::new(""));
        let mut count = 0;
        while frontier
            .next_pending(&root_fd, &normalizer, &exclusions, &WalkFaults::default())
            .expect("page")
            .is_some()
        {
            count += 1;
            assert!(frontier.max_page <= FRONTIER_PAGE);
        }
        assert_eq!(count, FRONTIER_PAGE * 3 + 17);
    }

    #[test]
    fn only_the_root_mechanism_directory_is_special() {
        let scratch = Scratch::new("walk-nested-mechanism-spelling");
        scratch.directory("vault/notes/.norn/tmp");
        scratch.place("notes/.norn/tmp/theirs.md", b"ordinary nested content");
        let facts = paths(walk(&scratch.at(""), &[]).expect("walk"));
        assert!(facts.contains(&(PathBuf::from("notes/.norn/tmp/theirs.md"), None)));
        assert!(
            !facts
                .iter()
                .any(|(_, reason)| matches!(reason, Some(SkipReason::Mechanism)))
        );
    }

    /// **The bar on a subtree walk.** Its facts are the vault walk's facts under
    /// that subtree, spelled from the vault root, so a caller can compare them
    /// against vault-relative state without repairing them first.
    ///
    /// The forbidden shape is a walk identified by where it starts. A subtree
    /// named `.norn` holding `tmp`, or any directory holding a `.norn/tmp` of
    /// its own, is vault content: reading it as the mechanism root would drop
    /// live documents from an inventory the vault walk includes.
    #[test]
    fn a_subtree_walk_yields_vault_relative_facts_and_the_vault_s_own_exclusions() {
        let scratch = Scratch::new("walk-subtree-vault-relative");
        scratch.directory("vault/notes/.norn/tmp");
        scratch.place("notes/.norn/tmp/theirs.md", b"ordinary nested content");
        scratch.place("notes/today.md", b"today");
        scratch.place("outside.md", b"outside");

        let facts = paths(walk_subtree(&scratch.at(""), Path::new("notes"), &[]).expect("walk"));
        assert_eq!(
            facts,
            vec![
                (PathBuf::from("notes/.norn/tmp/theirs.md"), None),
                (PathBuf::from("notes/today.md"), None),
            ]
        );
    }

    /// **The bar on the built-in root under a subtree walk.** The mechanism root
    /// hangs from the vault root wherever a walk starts, so a subtree walk of
    /// `.norn` cuts `tmp` without being told about it.
    #[test]
    fn a_subtree_walk_cuts_the_vault_s_mechanism_root() {
        let scratch = Scratch::new("walk-subtree-mechanism");
        scratch.directory("vault/.norn/tmp/deep");
        scratch.place(".norn/tmp/deep/staged", b"staged bytes");
        scratch.place(".norn/notes.md", b"a document under a dot directory");

        let facts = paths(walk_subtree(&scratch.at(""), Path::new(".norn"), &[]).expect("walk"));
        assert_eq!(
            facts,
            vec![
                (PathBuf::from(".norn/notes.md"), None),
                (PathBuf::from(".norn/tmp"), Some(SkipReason::Mechanism)),
            ]
        );
    }

    /// **The bar on an excluded subtree.** A walk of a root the vault excludes
    /// reads nothing under it and says so, rather than reporting an empty tree.
    #[test]
    fn a_subtree_walk_of_an_excluded_root_yields_only_its_skip() {
        let scratch = Scratch::new("walk-subtree-excluded");
        scratch.directory("vault/excluded/deep");
        scratch.place("excluded/deep/no.md", b"no");

        let facts = paths(
            walk_subtree(
                &scratch.at(""),
                Path::new("excluded/deep"),
                &[PathBuf::from("excluded")],
            )
            .expect("walk"),
        );
        assert_eq!(
            facts,
            vec![(
                PathBuf::from("excluded/deep"),
                Some(SkipReason::HostExclusion)
            )]
        );
    }

    /// **The bar on a subtree reached through a link.** The vault walk enters no
    /// symbolic link, so a subtree walk named through one reads nothing either
    /// and yields the same skip at the same name.
    ///
    /// The forbidden shape is resolving the subtree in one open, where
    /// `O_NOFOLLOW` binds only the last component: the walk would then enumerate
    /// files under a name the vault walk never admits, and every one of them
    /// would be spelled from a vault root it cannot be read through.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging a linked ancestor.
    fn a_subtree_walk_named_through_a_link_yields_the_link_s_own_skip() {
        let scratch = Scratch::new("walk-subtree-linked-ancestor");
        scratch.directory("vault/real/sub");
        scratch.place("real/sub/doc.md", b"a document under a linked name");
        std::os::unix::fs::symlink("real", scratch.at("link")).expect("link");

        let facts = paths(walk_subtree(&scratch.at(""), Path::new("link/sub"), &[]).expect("walk"));
        assert_eq!(
            facts,
            vec![(
                PathBuf::from("link"),
                Some(SkipReason::SymbolicLink(LinkKind::InVaultDirectory))
            )]
        );

        let itself = paths(walk_subtree(&scratch.at(""), Path::new("link"), &[]).expect("walk"));
        assert_eq!(itself, facts, "a linked subtree root is the same skip");
    }

    /// **The bar on a subtree walk's containment.** A link is judged against the
    /// vault root, so a target the vault contains is an in-vault name even when
    /// it sits outside the subtree the walk started at.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging a link out of the subtree.
    fn a_subtree_walk_judges_link_containment_against_the_vault_root() {
        let scratch = Scratch::new("walk-subtree-link-containment");
        scratch.directory("vault/notes");
        scratch.place("outside.md", b"outside");
        std::os::unix::fs::symlink("../outside.md", scratch.at("notes/up.md")).expect("link");

        let facts = paths(walk_subtree(&scratch.at(""), Path::new("notes"), &[]).expect("walk"));
        assert_eq!(
            facts,
            vec![(
                PathBuf::from("notes/up.md"),
                Some(SkipReason::SymbolicLink(LinkKind::InVaultFile))
            )]
        );
    }

    /// **The bar on deciding one name with no enumeration at all.** A caller
    /// holding a name asks the vault what the walk states in place of reaching
    /// it, and the answer is the subtree descent's: a shadow basename and a
    /// symbolic link each end it at their own name, an excluded root ends it at
    /// the path the root covers, and a name the walk reaches states nothing.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging a linked ancestor.
    fn a_vault_states_the_walk_s_skip_at_one_name() {
        let scratch = Scratch::new("vault-skip-reaching");
        scratch.directory("vault/dir/norn-shadow-7-2");
        scratch.directory("vault/real");
        scratch.directory("vault/excluded/deep");
        scratch.place("dir/norn-shadow-7-2/note.md", b"body");
        scratch.place("dir/plain.md", b"body");
        scratch.place("real/note.md", b"body");
        scratch.place("excluded/deep/note.md", b"body");
        std::os::unix::fs::symlink("../real", scratch.at("dir/link")).expect("link");

        let vault = Vault::open(&scratch.at(""), &[PathBuf::from("excluded")]).expect("a vault");
        let skip = |relative: &str| {
            vault
                .skip_reaching(Path::new(relative))
                .expect("a decided name")
                .map(|fact| (fact.path().as_path().to_owned(), fact.reason()))
        };
        assert_eq!(
            skip("dir/norn-shadow-7-2/note.md"),
            Some((PathBuf::from("dir/norn-shadow-7-2"), SkipReason::Shadow)),
            "a name under a shadow basename"
        );
        assert_eq!(
            skip("dir/norn-shadow-7-2"),
            Some((PathBuf::from("dir/norn-shadow-7-2"), SkipReason::Shadow)),
            "the shadow basename itself"
        );
        assert_eq!(
            skip("dir/link/note.md"),
            Some((
                PathBuf::from("dir/link"),
                SkipReason::SymbolicLink(LinkKind::InVaultDirectory)
            )),
            "a name under a symbolic link"
        );
        assert_eq!(
            skip("dir/link/norn-shadow-1-1"),
            Some((
                PathBuf::from("dir/link"),
                SkipReason::SymbolicLink(LinkKind::InVaultDirectory)
            )),
            "the link is reached before the last name is spelled"
        );
        assert_eq!(
            skip("excluded/deep/note.md"),
            Some((PathBuf::from("excluded"), SkipReason::HostExclusion)),
            "the excluded root, not the path that reached it"
        );
        assert_eq!(
            skip("dir/plain.md/under.md"),
            Some((
                PathBuf::from("dir/plain.md/under.md"),
                SkipReason::UnderAnEntry
            )),
            "a name below a document"
        );
        assert_eq!(skip("dir/plain.md"), None, "a file the walk reads");
        assert_eq!(skip("dir"), None, "a directory the walk enters");
    }

    /// **A name that is not there is not one of the walk's refusals.** No path
    /// the descent finds no name for holds anything, so the vault states no
    /// notation and leaves that whole reading to [`crate::path_kind`], which
    /// answers the same absence for the whole path.
    ///
    /// The last name's own spelling is read after the descent, not before it,
    /// so an absent ancestor answers for a shadow basename below it rather than
    /// the other way about. A shadow-spelled *ancestor* is read the same way:
    /// the descent stats the name before it judges the spelling, so a name no
    /// entry stands at holds nothing whatever it is spelled.
    #[test]
    fn a_name_that_is_not_there_is_no_skip_at_all() {
        let scratch = Scratch::new("vault-skip-absent");
        scratch.place("plain.md", b"body");

        let vault = Vault::open(&scratch.at(""), &[]).expect("a vault");
        for absent in [
            "gone",
            "gone/note.md",
            "gone/norn-shadow-1-1",
            "norn-shadow-3-3/x.md",
        ] {
            assert!(
                vault
                    .skip_reaching(Path::new(absent))
                    .expect("a decided name")
                    .is_none(),
                "{absent}"
            );
        }
    }

    /// **A name below an entry the walk reads is the vault's answer, not the
    /// caller's to work out.** Nothing stands under a document or under a
    /// device-like entry, and a stat of such a path refuses rather than
    /// reporting an absence, so the notation is stated here.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging a fifo component.
    fn a_name_below_an_entry_the_walk_reads_is_its_own_notation() {
        let scratch = Scratch::new("vault-skip-under-entry");
        scratch.place("plain.md", b"body");
        let made = std::process::Command::new("mkfifo")
            .arg(scratch.at("pipe"))
            .status()
            .expect("mkfifo runs");
        assert!(made.success(), "mkfifo failed");

        let vault = Vault::open(&scratch.at(""), &[]).expect("a vault");
        for blocked in ["plain.md/under.md", "pipe/under.md", "plain.md/a/b.md"] {
            assert_eq!(
                vault
                    .skip_reaching(Path::new(blocked))
                    .expect("a decided name")
                    .map(|fact| (fact.path().as_path().to_owned(), fact.reason())),
                Some((PathBuf::from(blocked), SkipReason::UnderAnEntry)),
                "{blocked}"
            );
        }
    }

    /// **Shadow-ness is an identity question, so the root's own case behavior
    /// decides it.** A root that resolves two spellings to one entry holds one
    /// name there, and the walk refuses that name however a caller spells it.
    ///
    /// A case-sensitive root holds two different names, and reads
    /// `NORN-SHADOW-7-2` as an ordinary directory it walks. That is the same
    /// rule giving the other answer, so the case states it rather than skipping
    /// silently.
    #[test]
    fn a_shadow_basename_is_refused_under_the_root_s_own_case_behavior() {
        let scratch = Scratch::new("walk-shadow-case");
        scratch.directory("vault/NORN-SHADOW-7-2");
        scratch.place("NORN-SHADOW-7-2/note.md", b"body");
        scratch.place("plain.md", b"body");

        let facts = paths(walk(&scratch.at(""), &[]).expect("walk"));
        let vault = Vault::open(&scratch.at(""), &[]).expect("a vault");
        let spelled_lower = vault
            .skip_reaching(Path::new("norn-shadow-7-2/note.md"))
            .expect("a decided name")
            .map(|fact| fact.reason());
        if vault.case_sensitivity() == CaseSensitivity::Sensitive {
            assert!(
                facts.contains(&(PathBuf::from("NORN-SHADOW-7-2/note.md"), None)),
                "a case-sensitive root holds a name no shadow spelling collides with"
            );
            assert_eq!(
                spelled_lower, None,
                "the lower spelling is a second name, and nothing is at it"
            );
            return;
        }
        assert_eq!(
            facts,
            vec![
                (PathBuf::from("NORN-SHADOW-7-2"), Some(SkipReason::Shadow)),
                (PathBuf::from("plain.md"), None),
            ],
            "the walk read through a name that folds onto a shadow basename"
        );
        assert_eq!(
            spelled_lower,
            Some(SkipReason::Shadow),
            "the other spelling of the one name this root holds"
        );
    }

    #[test]
    fn an_unreadable_later_directory_is_an_error_after_earlier_facts() {
        let scratch = Scratch::new("walk-streams-before-error");
        scratch.place("a.md", b"first");
        let blocked = scratch.directory("vault/z-blocked");
        scratch.place("z-blocked/no.md", b"unreadable");
        scratch.set_mode(&blocked, 0o000);

        #[allow(clippy::disallowed_methods)] // Proves this account is subject to the arranged mode.
        let actually_blocked = fs::read_dir(&blocked).is_err();
        assert!(
            actually_blocked,
            "this account can read a mode-000 directory, so the refusal case proves nothing"
        );

        let mut facts = walk(&scratch.at(""), &[]).expect("walk");
        assert_eq!(
            facts
                .next()
                .expect("first fact")
                .expect("the early file")
                .path()
                .as_path(),
            Path::new("a.md")
        );
        let error = facts
            .next()
            .expect("the blocked directory's position")
            .expect_err("an unreadable present directory");
        assert!(error.to_string().contains("z-blocked"), "{error}");
        assert!(facts.next().is_none(), "a walk error must be terminal");
        scratch.set_mode(&blocked, 0o755);
    }

    #[test]
    fn an_unreadable_present_file_refuses_when_content_is_requested() {
        let scratch = Scratch::new("walk-unreadable-file");
        let blocked = scratch.place("blocked.md", b"present but unreadable");
        scratch.set_mode(&blocked, 0o000);

        #[allow(clippy::disallowed_methods, clippy::disallowed_types)]
        // Proves the arranged read refusal is real.
        let actually_blocked = fs::File::open(&blocked).is_err();
        assert!(
            actually_blocked,
            "this account can read a mode-000 file, so the refusal case proves nothing"
        );

        let fact = walk(&scratch.at(""), &[])
            .expect("walk")
            .next()
            .expect("one fact")
            .expect("the present file is still a fact");
        let WalkFact::File(file) = fact else {
            panic!("the present file was treated as a skip")
        };
        // A denied file is the machine refusing, not the name answering: the
        // document is there and this account cannot read it.
        let error = file
            .read_optional()
            .expect_err("an unreadable present file");
        assert!(error.to_string().contains("blocked.md"), "{error}");
        scratch.set_mode(&blocked, 0o600);
    }

    /// **A denied ancestor is the machine refusing, not the name answering.**
    /// Permission revoked on a directory between the walk that enumerated a file
    /// under it and the open that reads it says nothing about whether the
    /// document is there — and reading it as absence would let a permission loss
    /// prune every row beneath that directory.
    ///
    /// This is the half of the absence/refusal split the deletion and
    /// replacement cases do not reach: those revoke the *name*, and this revokes
    /// the *account's* reach on a name that never moved.
    #[test]
    fn a_denied_ancestor_directory_refuses_the_open_it_stands_over() {
        let scratch = Scratch::new("walk-unreadable-ancestor");
        let folder = scratch.directory("vault/folder");
        scratch.place("folder/note.md", b"present under a directory");

        let fact = walk(&scratch.at(""), &[])
            .expect("walk")
            .find_map(|fact| match fact.expect("the enumerated file") {
                WalkFact::File(file) => Some(file),
                WalkFact::Skipped(_) => None,
            })
            .expect("one file fact");
        scratch.set_mode(&folder, 0o000);

        #[allow(clippy::disallowed_methods, clippy::disallowed_types)]
        // Proves the arranged refusal is real for this account.
        let actually_blocked = fs::File::open(scratch.at("folder/note.md")).is_err();
        assert!(
            actually_blocked,
            "this account reads through a mode-000 directory, so the refusal case proves nothing"
        );

        // The mode goes back before the assertion, so a failure here leaves a
        // readable tree behind rather than an undeletable one.
        let outcome = fact.read_optional();
        scratch.set_mode(&folder, 0o755);
        let Err(error) = outcome else {
            panic!("a document under a denied ancestor read as an absence")
        };
        assert!(error.to_string().contains("note.md"), "{error}");
    }
}
