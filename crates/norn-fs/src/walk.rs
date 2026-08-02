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

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, open, openat, readlinkat, statat};

use crate::hash::{ContentHash, read_bytes_and_hash};
use crate::identity::Identity;
use crate::path::{NormalizedPath, NormalizerError, PathError, PathNormalizer};
use crate::shadow::{FALLBACK, is_shadow_name};

/// Begins a deterministic streaming walk of `root`.
///
/// Construction proves the root's case behavior, validates every exclusion,
/// and opens only the first directory frontier. Later directory failures arrive
/// from the iterator at their deterministic position.
pub fn walk(root: &Path, exclusions: &[PathBuf]) -> Result<Walk, WalkError> {
    let normalizer = PathNormalizer::detect(root).map_err(WalkError::Normalizer)?;
    let exclusions = exclusions
        .iter()
        .map(|path| {
            normalizer
                .normalize(path)
                .map_err(|source| WalkError::Path {
                    path: path.clone(),
                    source,
                })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let mechanism = normalizer
        .normalize(Path::new(FALLBACK))
        .expect("the fixed mechanism root is relative and normalized");
    let root_fd = Arc::new(
        open(root, directory_flags(), Mode::empty())
            .map_err(|source| environment_errno("opening directory", root, source))?,
    );
    let first = DirectoryFrontier::new(root_fd.clone(), Path::new(""));
    Ok(Walk {
        root: Arc::new(root.to_owned()),
        root_fd,
        normalizer,
        exclusions,
        mechanism,
        stack: vec![first],
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
    exclusions: BTreeSet<NormalizedPath>,
    mechanism: NormalizedPath,
    stack: Vec<DirectoryFrontier>,
}

impl Iterator for Walk {
    type Item = Result<WalkFact, WalkError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let frontier = self.stack.last_mut()?;
            let pending = match frontier.next_pending(
                &self.root_fd,
                &self.normalizer,
                &self.exclusions,
                &self.mechanism,
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

    /// Opens once, reads the bytes once, and hashes exactly those returned bytes.
    ///
    /// Consuming the fact makes a second read through this observation
    /// unspellable. The returned stat comes from the held descriptor and may
    /// differ from the traversal-time stat if another writer replaced the name.
    #[allow(clippy::disallowed_methods, clippy::disallowed_types)] // norn-fs owns vault handles and reads.
    pub fn read(self) -> Result<ReadFile, WalkError> {
        let access = self.root.join(self.path.as_path());
        let fd = open_regular(&self.root_fd, self.path.as_path())
            .map_err(|source| environment_errno("opening", &access, source))?;
        let mut file = fs::File::from(fd);
        let metadata = file
            .metadata()
            .map_err(|source| environment("stating", &access, source))?;
        if !metadata.is_file() {
            return Err(environment(
                "reading",
                &access,
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the name no longer identifies a regular file",
                ),
            ));
        }
        let (bytes, content_hash) = read_bytes_and_hash(&mut file)
            .map_err(|source| environment("reading", &access, source))?;
        Ok(ReadFile {
            path: self.path,
            stat: stat(&metadata),
            bytes,
            content_hash,
        })
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
    sort_key: Vec<u8>,
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

    fn next_pending(
        &mut self,
        root_fd: &Arc<OwnedFd>,
        normalizer: &PathNormalizer,
        exclusions: &BTreeSet<NormalizedPath>,
        mechanism: &NormalizedPath,
    ) -> Result<Option<Pending>, WalkError> {
        if let Some(item) = self.page.next() {
            return Ok(Some(item));
        }
        if self.done {
            return Ok(None);
        }
        let (items, more) = match directory_page(
            root_fd,
            &self.fd,
            &self.relative,
            self.cursor.as_deref(),
            normalizer,
            exclusions,
            mechanism,
        ) {
            Ok(page) => page,
            Err(error) => {
                self.done = true;
                return Err(error);
            }
        };
        self.done = !more;
        #[cfg(test)]
        {
            self.max_page = self.max_page.max(items.len());
        }
        self.cursor = items.last().map(|item| item.sort_key.clone());
        self.page = items.into_iter();
        Ok(self.page.next())
    }
}

#[allow(clippy::disallowed_methods)] // norn-fs owns the vault walk and stat.
fn directory_page(
    root_fd: &Arc<OwnedFd>,
    directory: &Arc<OwnedFd>,
    relative: &Path,
    cursor: Option<&[u8]>,
    normalizer: &PathNormalizer,
    exclusions: &BTreeSet<NormalizedPath>,
    mechanism: &NormalizedPath,
) -> Result<(Vec<Pending>, bool), WalkError> {
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
        let name = OsString::from_vec(name.to_vec());
        let relative_path = relative.join(&name);
        let path = normalizer
            .normalize(&relative_path)
            .map_err(|source| WalkError::Path {
                path: relative_path.clone(),
                source,
            })?;
        let kind = if entry.file_type() == FileType::Unknown {
            let metadata = statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|source| environment_errno("stating", &relative_path, source))?;
            classify_file_type(FileType::from_raw_mode(metadata.st_mode as _))
        } else {
            classify_file_type(entry.file_type())
        };
        let mut sort_key = path.comparison_key().as_bytes().to_vec();
        if kind == EntryKind::Directory
            && !exclusions.contains(&path)
            && path != *mechanism
            && !is_shadow_name(&name)
        {
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

    let mut pending = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let Candidate {
            name,
            path,
            kind,
            sort_key,
        } = candidate;
        let relative_path = relative.join(&name);
        let metadata = statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|source| environment_errno("stating", &relative_path, source))?;
        let observed_kind = classify_file_type(FileType::from_raw_mode(metadata.st_mode as _));
        if observed_kind != kind {
            return Err(environment(
                "paging directory entry",
                &relative_path,
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the entry changed kind during directory enumeration",
                ),
            ));
        }
        let skip = if exclusions.contains(&path) {
            Some(SkipReason::HostExclusion)
        } else if path == *mechanism {
            Some(SkipReason::Mechanism)
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
            sort_key,
        });
    }
    Ok((pending, more))
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

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY | OFlags::NOFOLLOW
}

fn open_regular(root: &Arc<OwnedFd>, relative: &Path) -> Result<OwnedFd, rustix::io::Errno> {
    let mut directory = root.clone();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let name = component.as_os_str();
        if components.peek().is_some() {
            directory = Arc::new(openat(&directory, name, directory_flags(), Mode::empty())?);
            continue;
        }
        let fd = openat(
            &directory,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        )?;
        let metadata = rustix::fs::fstat(&fd)?;
        if FileType::from_raw_mode(metadata.st_mode as _) != FileType::RegularFile {
            return Err(rustix::io::Errno::INVAL);
        }
        return Ok(fd);
    }
    Err(rustix::io::Errno::INVAL)
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
        identity: Identity {
            dev: metadata.dev(),
            ino: metadata.ino(),
        },
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
    use super::*;
    use crate::scratch::Scratch;
    use std::io::{Cursor, Read};

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
        let read = file.read().expect("read");
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

    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: replacing a deferred name.
    fn a_deferred_file_replaced_by_a_fifo_refuses_without_blocking() {
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

        let error = file.read().expect_err("a fifo is not document content");
        assert!(error.to_string().contains("note.md"), "{error}");
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
        let mechanism = normalizer
            .normalize(Path::new(FALLBACK))
            .expect("mechanism");
        let mut frontier = DirectoryFrontier::new(root_fd.clone(), Path::new(""));
        let mut count = 0;
        while frontier
            .next_pending(&root_fd, &normalizer, &BTreeSet::new(), &mechanism)
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
        let error = file.read().expect_err("an unreadable present file");
        assert!(error.to_string().contains("blocked.md"), "{error}");
        scratch.set_mode(&blocked, 0o600);
    }
}
