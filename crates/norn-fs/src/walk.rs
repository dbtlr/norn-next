#![forbid(unsafe_code)]
//! A streaming inventory of one vault tree.
//!
//! The walk keeps one sorted directory frontier per depth rather than a tree of
//! every path. That makes its memory a function of directory depth and maximum
//! fan-out, not of the number of documents in the vault. Files and typed skip
//! notations are yielded in normalized-path lexicographic order; directories
//! themselves are traversal machinery and are not facts.
//!
//! Symbolic links are facts about names, never traversal edges. Host exclusions
//! are roots, not schema rules, and Norn's cross-device shadow fallback is the
//! one built-in mechanism root. Ordinary dotfiles and dot-directories belong to
//! the vault and are walked normally.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::hash::ContentHash;
use crate::identity::Identity;
use crate::path::{NormalizedPath, NormalizerError, PathError, PathNormalizer};
use crate::shadow::{FALLBACK, is_shadow_name};

/// Host-owned roots the filesystem seam must not enter.
#[derive(Clone, Debug, Default)]
pub struct WalkOptions {
    exclusions: Vec<PathBuf>,
}

impl WalkOptions {
    /// Builds options from vault-relative exclusion roots.
    pub fn new(exclusions: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            exclusions: exclusions.into_iter().collect(),
        }
    }
}

/// Begins a deterministic streaming walk of `root`.
///
/// Construction proves the root's case behavior, validates every exclusion,
/// and opens only the first directory frontier. Later directory failures arrive
/// from the iterator at their deterministic position.
pub fn walk(root: &Path, options: WalkOptions) -> Result<Walk, WalkError> {
    let normalizer = PathNormalizer::detect(root).map_err(WalkError::Normalizer)?;
    let exclusions = options
        .exclusions
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
    let first = directory_entries(
        root,
        root,
        Path::new(""),
        &normalizer,
        &exclusions,
        &mechanism,
    )?;
    Ok(Walk {
        root: root.to_owned(),
        normalizer,
        exclusions,
        mechanism,
        stack: vec![first.into_iter()],
    })
}

/// A streaming vault walk.
pub struct Walk {
    root: PathBuf,
    normalizer: PathNormalizer,
    exclusions: BTreeSet<NormalizedPath>,
    mechanism: NormalizedPath,
    stack: Vec<std::vec::IntoIter<Pending>>,
}

impl Iterator for Walk {
    type Item = Result<WalkFact, WalkError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let frontier = self.stack.last_mut()?;
            let Some(pending) = frontier.next() else {
                self.stack.pop();
                continue;
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
                        path: pending.path,
                        stat: pending.stat,
                    })));
                }
                EntryKind::Directory => {
                    let access = self.root.join(pending.path.as_path());
                    match directory_entries(
                        &self.root,
                        &access,
                        pending.path.as_path(),
                        &self.normalizer,
                        &self.exclusions,
                        &self.mechanism,
                    ) {
                        Ok(entries) => self.stack.push(entries.into_iter()),
                        Err(error) => return Some(Err(error)),
                    }
                }
                EntryKind::Symlink => unreachable!("every symbolic link is a skip"),
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
    root: PathBuf,
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
        let mut file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&access)
            .map_err(|source| environment("opening", &access, source))?;
        let metadata = file
            .metadata()
            .map_err(|source| environment("stating", &access, source))?;
        let (bytes, content_hash) =
            read_and_hash(&mut file).map_err(|source| environment("reading", &access, source))?;
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
    pub fn path(&self) -> &NormalizedPath {
        &self.path
    }

    pub fn stat(&self) -> &FileStat {
        &self.stat
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

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
    pub fn path(&self) -> &NormalizedPath {
        &self.path
    }

    pub fn reason(&self) -> SkipReason {
        self.reason
    }
}

/// Why a root has one notation and no descendant facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkipReason {
    HostExclusion,
    Mechanism,
    Shadow,
    SymbolicLink(LinkKind),
}

/// What an unsupported symbolic link was observed to name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkKind {
    InVaultFile,
    InVaultDirectory,
    Dangling,
    Outbound,
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
}

struct Pending {
    path: NormalizedPath,
    kind: EntryKind,
    stat: FileStat,
    skip: Option<SkipReason>,
    sort_key: Vec<u8>,
}

#[allow(clippy::disallowed_methods)] // norn-fs owns the vault walk and stat.
fn directory_entries(
    root: &Path,
    directory: &Path,
    relative: &Path,
    normalizer: &PathNormalizer,
    exclusions: &BTreeSet<NormalizedPath>,
    mechanism: &NormalizedPath,
) -> Result<Vec<Pending>, WalkError> {
    let entries = fs::read_dir(directory)
        .map_err(|source| environment("reading directory", directory, source))?;
    let mut pending = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| environment("reading entry in", directory, source))?;
        let relative = relative.join(entry.file_name());
        let path = normalizer
            .normalize(&relative)
            .map_err(|source| WalkError::Path {
                path: relative.clone(),
                source,
            })?;
        let file_type = entry
            .file_type()
            .map_err(|source| environment("typing", &entry.path(), source))?;
        let kind = if file_type.is_symlink() {
            EntryKind::Symlink
        } else if file_type.is_dir() {
            EntryKind::Directory
        } else {
            EntryKind::File
        };
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|source| environment("stating", &entry.path(), source))?;
        let skip = if exclusions.contains(&path) {
            Some(SkipReason::HostExclusion)
        } else if path == *mechanism {
            Some(SkipReason::Mechanism)
        } else if is_shadow_name(entry.file_name().as_os_str()) {
            Some(SkipReason::Shadow)
        } else if kind == EntryKind::Symlink {
            Some(SkipReason::SymbolicLink(classify_link(
                root,
                path.as_path(),
                &entry.path(),
                normalizer,
            )?))
        } else {
            None
        };
        let mut sort_key = path.comparison_key().as_bytes().to_vec();
        if kind == EntryKind::Directory && skip.is_none() {
            sort_key.push(b'/');
        }
        pending.push(Pending {
            path,
            kind,
            stat: stat(&metadata),
            skip,
            sort_key,
        });
    }
    pending.sort_by(|left, right| left.sort_key.cmp(&right.sort_key));
    Ok(pending)
}

#[allow(clippy::disallowed_methods)] // norn-fs owns vault links and stat.
fn classify_link(
    root: &Path,
    relative_link: &Path,
    link: &Path,
    normalizer: &PathNormalizer,
) -> Result<LinkKind, WalkError> {
    let target = fs::read_link(link).map_err(|source| environment("reading link", link, source))?;
    if target.is_absolute() {
        return Ok(LinkKind::Outbound);
    }
    let Some(relative) = resolve_relative_target(relative_link, &target) else {
        return Ok(LinkKind::Outbound);
    };
    let relative = normalizer
        .normalize(&relative)
        .expect("the lexical resolver returns a non-empty relative path without parents");
    let candidate = root.join(relative.as_path());
    match fs::metadata(candidate) {
        Ok(metadata) if metadata.is_dir() => Ok(LinkKind::InVaultDirectory),
        Ok(_) => Ok(LinkKind::InVaultFile),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(LinkKind::Dangling),
        Err(source) => Err(environment("stating link target for", link, source)),
    }
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
    (!parts.is_empty()).then(|| parts.into_iter().collect())
}

/// One forward pass over `reader`; parsers receive the same bytes the hash saw.
fn read_and_hash(reader: &mut impl Read) -> io::Result<(Vec<u8>, ContentHash)> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    let hash = ContentHash::of(&bytes);
    Ok((bytes, hash))
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
    use std::io::Cursor;

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

        let facts = paths(walk(&scratch.at(""), WalkOptions::default()).expect("walk"));
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

        let facts = paths(
            walk(
                &scratch.at(""),
                WalkOptions::new([PathBuf::from("./excluded")]),
            )
            .expect("walk"),
        );
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
        let fact = walk(&scratch.at(""), WalkOptions::default())
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
        let (bytes, hash) = read_and_hash(&mut reader).expect("one reading");
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

        let facts = paths(walk(&scratch.at(""), WalkOptions::default()).expect("walk"));
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
    fn only_the_root_mechanism_directory_is_special() {
        let scratch = Scratch::new("walk-nested-mechanism-spelling");
        scratch.directory("vault/notes/.norn/tmp");
        scratch.place("notes/.norn/tmp/theirs.md", b"ordinary nested content");
        let facts = paths(walk(&scratch.at(""), WalkOptions::default()).expect("walk"));
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

        let mut facts = walk(&scratch.at(""), WalkOptions::default()).expect("walk");
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

        let fact = walk(&scratch.at(""), WalkOptions::default())
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
