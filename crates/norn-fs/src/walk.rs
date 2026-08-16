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
//! **Every window in the walk converges on absence.** Listing a directory,
//! stating the names it listed, and opening a file it yielded are separate
//! observations of a tree other writers are editing, and an entry can be
//! unlinked — or unlinked and replaced — between any two of them. One doctrine
//! answers all of them: the entry is dropped, because a walk begun now holds
//! nothing at that name either. A machine that will not answer is the other
//! thing entirely, and still refuses.

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
use crate::path::{NormalizedPath, NormalizerError, PathError, PathNormalizer};
use crate::shadow::is_shadow_name;

/// Begins a deterministic streaming walk of `root`.
///
/// Construction proves the root's case behavior, validates every exclusion,
/// and opens only the first directory frontier. Later directory failures arrive
/// from the iterator at their deterministic position.
pub fn walk(root: &Path, exclusions: &[PathBuf]) -> Result<Walk, WalkError> {
    let mut walk = open_vault(root, exclusions)?;
    let first = DirectoryFrontier::new(walk.root_fd.clone(), Path::new(""));
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
    let mut walk = open_vault(root, exclusions)?;
    let subtree = walk
        .normalizer
        .normalize(relative)
        .map_err(|source| WalkError::Path {
            path: relative.to_owned(),
            source,
        })?;
    if let Some(reason) = walk.exclusions.reason(&subtree) {
        walk.skipped = Some(SkipFact {
            path: subtree,
            reason: reason.into(),
        });
        return Ok(walk);
    }
    match open_subtree(&walk, &subtree)? {
        Frontier::Open(fd) => walk
            .stack
            .push(DirectoryFrontier::new(fd, subtree.as_path())),
        Frontier::Skipped(skipped) => walk.skipped = Some(skipped),
    }
    Ok(walk)
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
#[allow(clippy::disallowed_methods)] // norn-fs owns the vault walk and stat.
fn open_subtree(walk: &Walk, subtree: &NormalizedPath) -> Result<Frontier, WalkError> {
    let mut directory = walk.root_fd.clone();
    let mut traversed = PathBuf::new();
    for component in subtree.as_path().components() {
        let name = component.as_os_str();
        traversed.push(name);
        let access = walk.root.join(&traversed);
        let path = walk
            .normalizer
            .normalize(&traversed)
            .expect("a leading run of a normalized path's components is normalized");
        if is_shadow_name(name) {
            return Ok(Frontier::Skipped(SkipFact {
                path,
                reason: SkipReason::Shadow,
            }));
        }
        crate::reads::count_stat();
        let metadata = statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|source| environment_errno("stating", &access, source))?;
        if classify_file_type(FileType::from_raw_mode(metadata.st_mode as _)) == EntryKind::Symlink
        {
            let kind = classify_link(
                &walk.root_fd,
                path.as_path(),
                &directory,
                name,
                &walk.normalizer,
            )?;
            return Ok(Frontier::Skipped(SkipFact {
                path,
                reason: SkipReason::SymbolicLink(kind),
            }));
        }
        directory = Arc::new(
            openat(&directory, name, directory_flags(), Mode::empty())
                .map_err(|source| environment_errno("opening directory", &access, source))?,
        );
    }
    Ok(Frontier::Open(directory))
}

/// The vault-identified walk state both entry points start from, with no
/// frontier open yet.
fn open_vault(root: &Path, exclusions: &[PathBuf]) -> Result<Walk, WalkError> {
    let normalizer = PathNormalizer::detect(root).map_err(WalkError::Normalizer)?;
    let exclusions = Exclusions::new(&normalizer, exclusions)?;
    let root_fd = Arc::new(
        open(root, directory_flags(), Mode::empty())
            .map_err(|source| environment_errno("opening directory", root, source))?,
    );
    Ok(Walk {
        root: Arc::new(root.to_owned()),
        root_fd,
        normalizer,
        exclusions,
        stack: Vec::new(),
        skipped: None,
        faults: WalkFaults::entry(),
    })
}

/// A streaming vault walk.
///
/// Any yielded error is terminal: later calls return `None`, so exhaustion can
/// describe a complete inventory only when every preceding item was `Ok`.
pub struct Walk {
    root: Arc<PathBuf>,
    root_fd: Arc<OwnedFd>,
    normalizer: PathNormalizer,
    exclusions: Exclusions,
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
                &self.root_fd,
                &self.normalizer,
                &self.exclusions,
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

            if let Some(reason) = pending.skip {
                return Some(Ok(WalkFact::Skipped(SkipFact {
                    path: pending.path,
                    reason,
                })));
            }
            match pending.kind {
                EntryKind::File => {
                    return Some(Ok(WalkFact::File(FileFact {
                        root: self.root.clone(),
                        root_fd: self.root_fd.clone(),
                        path: pending.path,
                        stat: pending.stat,
                    })));
                }
                EntryKind::Directory => {
                    let access = self.root.join(pending.path.as_path());
                    match openat(
                        &pending.parent,
                        &pending.name,
                        directory_flags(),
                        Mode::empty(),
                    ) {
                        Ok(fd) => self
                            .stack
                            .push(DirectoryFrontier::new(Arc::new(fd), pending.path.as_path())),
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
                EntryKind::Symlink | EntryKind::Special(_) => {
                    unreachable!("unsupported entries are skips")
                }
            }
        }
    }
}

impl Walk {
    /// The path ordering proven for this walk's root.
    pub fn case_sensitivity(&self) -> crate::CaseSensitivity {
        self.normalizer.case_sensitivity()
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

/// One root deliberately omitted from traversal.
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
}

/// Why a root has one notation and no descendant facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkipReason {
    /// A root supplied by the host.
    HostExclusion,
    /// Norn's exact `.norn/tmp` fallback subtree.
    Mechanism,
    /// An exact Norn shadow basename.
    Shadow,
    /// A symbolic link, which is never traversed.
    SymbolicLink(LinkKind),
    /// A device-like entry unsafe to open as document content.
    SpecialFile(FileKind),
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
    kind: EntryKind,
    stat: FileStat,
    skip: Option<SkipReason>,
}

struct Candidate {
    name: OsString,
    path: NormalizedPath,
    kind: EntryKind,
    sort_key: Vec<u8>,
}

const FRONTIER_PAGE: usize = 256;

struct DirectoryFrontier {
    fd: Arc<OwnedFd>,
    relative: PathBuf,
    cursor: Option<Vec<u8>>,
    page: std::vec::IntoIter<Pending>,
    done: bool,
    #[cfg(test)]
    max_page: usize,
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
        }
    }

    /// The next entry this directory holds, paging the directory again where the
    /// page in hand is spent.
    ///
    /// **A page can come back holding fewer entries than it covered, and an
    /// exhausted page is not an exhausted directory.** Entries that vanish
    /// between the listing and their stat are dropped, so a page whose every
    /// entry went away is empty while the directory still has entries past it —
    /// and reading that as the end would take the whole tail of the directory
    /// with it. The loop asks for the next page instead, and the cursor each page
    /// leaves is the run of names it covered rather than the last one it kept, so
    /// a page that dropped everything still advances.
    fn next_pending(
        &mut self,
        root_fd: &Arc<OwnedFd>,
        normalizer: &PathNormalizer,
        exclusions: &Exclusions,
        faults: &WalkFaults,
    ) -> Result<Option<Pending>, WalkError> {
        loop {
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
            }
            if let Some(covered) = page.covered {
                self.cursor = Some(covered);
            }
            self.page = page.pending.into_iter();
        }
    }
}

/// One bounded page of a directory's entries.
struct Page {
    /// The entries the page kept, in order.
    pending: Vec<Pending>,
    /// Whether the directory holds names past the ones this page covered.
    more: bool,
    /// The highest sort key this page covered, which is where the next page of
    /// the same directory starts. It is the run of names the page took in, so it
    /// stands whether or not the entry holding it survived to be yielded.
    covered: Option<Vec<u8>>,
}

/// One page of `directory`, listed and then stat'd.
///
/// **The listing and the stats are two observations, and the page converges on
/// what the second one holds.** A name the listing returned can be unlinked
/// before its stat runs, and it can be unlinked and taken by an entry of another
/// kind; both are one edit to a tree other writers share, and both are answered
/// by dropping the entry from the page. That is the answer a walk begun now
/// gives — it lists no such entry — so a heal reading this page converges on the
/// same state either way, and the successor an edit left behind is carried by
/// the watcher events that edit raised.
///
/// **A machine that will not answer still refuses.** A denied stat, an exhausted
/// descriptor table and a failing device say nothing about whether an entry is
/// there, and reading one as absence would let a transient fault prune derived
/// state. Absence is the one error number that converges; every other one ends
/// the walk.
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
    let mut candidates = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|source| environment_errno("reading entry in", display, source))?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        crate::reads::count_dirents(1);
        let name = OsString::from_vec(name.to_vec());
        let relative_path = relative.join(&name);
        let path = normalizer
            .normalize(&relative_path)
            .map_err(|source| WalkError::Path {
                path: relative_path.clone(),
                source,
            })?;
        // A filesystem whose listing carries no entry type is stat'd to learn
        // one, which opens the same window one call earlier: a name gone by the
        // time this runs is an entry the page never takes in. It is the same
        // doctrine through the same reading, so the two windows cannot answer a
        // deleted name differently.
        let kind = if entry.file_type() == FileType::Unknown {
            match stat_listed(directory, &name, &relative_path, Paged::default())? {
                Some(metadata) => {
                    classify_file_type(FileType::from_raw_mode(metadata.st_mode as _))
                }
                None => continue,
            }
        } else {
            classify_file_type(entry.file_type())
        };
        let mut sort_key = path.comparison_key().as_bytes().to_vec();
        if kind == EntryKind::Directory && !exclusions.excludes(&path) && !is_shadow_name(&name) {
            sort_key.push(b'/');
        }
        if cursor.is_some_and(|cursor| sort_key.as_slice() <= cursor) {
            continue;
        }
        candidates.push(Candidate {
            name,
            path,
            kind,
            sort_key,
        });
        candidates.sort_by(|left, right| left.sort_key.cmp(&right.sort_key));
        if candidates.len() > FRONTIER_PAGE + 1 {
            candidates.pop();
        }
    }
    candidates.sort_by(|left, right| left.sort_key.cmp(&right.sort_key));
    let more = candidates.len() > FRONTIER_PAGE;
    candidates.truncate(FRONTIER_PAGE);
    let covered = candidates.last().map(|last| last.sort_key.clone());

    let mut pending = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let Candidate {
            name,
            path,
            kind,
            sort_key: _,
        } = candidate;
        let relative_path = relative.join(&name);
        let armed = faults.paging(kind);
        let Some(metadata) = stat_listed(directory, &name, &relative_path, armed)? else {
            continue;
        };
        let observed_kind = armed
            .observes
            .unwrap_or_else(|| classify_file_type(FileType::from_raw_mode(metadata.st_mode as _)));
        // The name the listing named was taken away and given to something else
        // inside this window. What stands there is not the entry that was
        // listed, so the page drops it exactly as it drops a name that is simply
        // gone — and the successor arrives as the change it is, through the
        // watcher events that edit raised.
        if observed_kind != kind {
            continue;
        }
        let skip = if let Some(reason) = exclusions.reason(&path) {
            Some(reason.into())
        } else if is_shadow_name(&name) {
            Some(SkipReason::Shadow)
        } else if kind == EntryKind::Symlink {
            Some(SkipReason::SymbolicLink(classify_link(
                root_fd,
                path.as_path(),
                directory,
                &name,
                normalizer,
            )?))
        } else if let EntryKind::Special(kind) = kind {
            Some(SkipReason::SpecialFile(kind))
        } else {
            None
        };
        pending.push(Pending {
            parent: directory.clone(),
            name,
            path,
            kind,
            stat: stat_raw(&metadata),
            skip,
        });
    }
    Ok(Page {
        pending,
        more,
        covered,
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

#[allow(clippy::disallowed_methods)] // norn-fs owns vault links and stat.
fn classify_link(
    root_fd: &Arc<OwnedFd>,
    relative_link: &Path,
    parent: &Arc<OwnedFd>,
    name: &OsStr,
    normalizer: &PathNormalizer,
) -> Result<LinkKind, WalkError> {
    let target = readlinkat(parent, name, Vec::new())
        .map_err(|source| environment_errno("reading link", relative_link, source))?;
    let target = PathBuf::from(OsString::from_vec(target.into_bytes()));
    if target.is_absolute() {
        return Ok(LinkKind::Outbound);
    }
    let Some(relative) = resolve_relative_target(relative_link, &target) else {
        return Ok(LinkKind::Outbound);
    };
    if relative.as_os_str().is_empty() {
        return Ok(LinkKind::InVaultDirectory);
    }
    let relative = normalizer
        .normalize(&relative)
        .expect("the lexical resolver returns a non-empty relative path without parents");
    inspect_link_target(root_fd, relative.as_path())
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

        let error = facts
            .next()
            .expect("pending position")
            .expect_err("nofollow refuses replacement");
        assert!(error.to_string().contains("z-pending"), "{error}");
        assert!(
            facts.all(|fact| fact.expect("later fact").path().as_path()
                != Path::new("z-pending/canary.md"))
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
    /// is no entry there. The page drops it and keeps going.
    ///
    /// The forbidden shape is a refusal, which reports one foreign deletion as a
    /// broken environment and takes the whole enumeration — every sibling
    /// included — down with it.
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
            vec![(PathBuf::from("b.md"), None), (PathBuf::from("c.md"), None)]
        );
    }

    /// **A kind change in the same window is the same vanishing.** The name the
    /// listing named was unlinked and something else took it, so what stands
    /// there is not the entry that was listed — and the page drops it exactly as
    /// it drops a name that is simply gone.
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
            vec![(PathBuf::from("b.md"), None), (PathBuf::from("c.md"), None)]
        );
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

    /// **A page that dropped an entry still covers the run of names it took
    /// in.** The next page of the same directory starts after the last name this
    /// one listed, not after the last one it kept — so an entry that vanishes
    /// neither hides the names between it and the page boundary nor yields the
    /// page's own tail twice.
    #[test]
    fn a_page_that_dropped_an_entry_pages_the_rest_of_the_directory_once() {
        let scratch = Scratch::new("walk-page-vanishes-wide");
        for index in 0..(FRONTIER_PAGE + 1) {
            scratch.place(&format!("{index:04}.md"), b"x");
        }

        let facts = paths(walk_armed(
            &scratch.at(""),
            &[(Stage::Page, Answer::Vanishes)],
        ));
        let expected: Vec<(PathBuf, Option<SkipReason>)> = (1..(FRONTIER_PAGE + 1))
            .map(|index| (PathBuf::from(format!("{index:04}.md")), None))
            .collect();
        assert_eq!(facts, expected);
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
