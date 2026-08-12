//! Atomic observation of one file below one anchor directory.

use std::io;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};

use rustix::fs::{Mode, OFlags, open};

use crate::hash::{ContentHash, read_bytes_and_hash};
use crate::open::{Reached, Unreached, open_regular_at};
use crate::refusal::{Refusal, environment};

/// What kind of filesystem object a watcher invalidation root names now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathKind {
    Missing,
    RegularFile,
    Directory,
    Other,
}

#[allow(clippy::disallowed_methods)]
pub fn path_kind(path: &Path) -> Result<PathKind, Refusal> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(PathKind::Missing),
        Err(error) => return Err(environment("stating", path, &error)),
    };
    let kind = metadata.file_type();
    Ok(if kind.is_file() {
        PathKind::RegularFile
    } else if kind.is_dir() {
        PathKind::Directory
    } else {
        PathKind::Other
    })
}

/// Bytes and their content hash from one read of one held file descriptor.
#[derive(Clone, Debug)]
pub struct ReadAndHash {
    path: PathBuf,
    bytes: Vec<u8>,
    content_hash: ContentHash,
}

impl ReadAndHash {
    /// The path that was opened, spelled as the caller named it: the anchor
    /// with the relative name below it.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The bytes read from the held descriptor.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The content hash computed from exactly [`Self::bytes`].
    pub fn content_hash(&self) -> ContentHash {
        self.content_hash
    }

    /// Splits the observation into parser input and its fingerprint.
    pub fn into_parts(self) -> (Vec<u8>, ContentHash) {
        (self.bytes, self.content_hash)
    }
}

/// Reads the regular file `relative` names below `anchor`, once.
///
/// The read is [contained](crate::open): no component of `relative` is followed
/// through a symbolic link, and a name that is not a regular file is refused
/// rather than opened for content. A caller that wants an absent name to be an
/// answer rather than a refusal wants [`read_optional_and_hash`].
///
/// `anchor` itself is resolved as the caller spelled it. It is the boundary the
/// read cannot reach past, not a name this crate re-derives.
pub fn read_and_hash(anchor: &Path, relative: &Path) -> Result<ReadAndHash, Refusal> {
    let path = anchor.join(relative);
    match observe(anchor, relative, &path)? {
        Observed::Read(read) => Ok(read),
        Observed::Nothing(unreached) => {
            Err(environment(unreached.operation(), &path, unreached.error()))
        }
    }
}

/// Reads the regular file `relative` names below `anchor`, or answers that
/// there is no such file to read.
///
/// Same containment as [`read_and_hash`], and the difference is what a name
/// that reaches no regular file means. Here it is an answer: a deletion the
/// watcher is telling the caller about, a name that turned into a pipe or a
/// directory, and a name reached only through a symbolic link are all *nothing
/// to read*, and they are one answer because a caller converging on what a
/// walk of the anchor holds converges the same way for every one of them — a
/// walk yields no file at any of those names either.
///
/// A machine failure is still a refusal. A directory this account cannot open
/// and a descriptor table that is full say nothing about whether a document is
/// there, and reporting them as absence would let a transient fault delete
/// derived state.
pub fn read_optional_and_hash(
    anchor: &Path,
    relative: &Path,
) -> Result<Option<ReadAndHash>, Refusal> {
    let path = anchor.join(relative);
    Ok(match observe(anchor, relative, &path)? {
        Observed::Read(read) => Some(read),
        Observed::Nothing(_) => None,
    })
}

/// One observation, before either caller decides what an unreached name means.
enum Observed {
    Read(ReadAndHash),
    Nothing(Unreached),
}

#[allow(clippy::disallowed_types)] // norn-fs owns file handles.
fn observe(anchor: &Path, relative: &Path, path: &Path) -> Result<Observed, Refusal> {
    let anchor_fd = open(
        anchor,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY,
        Mode::empty(),
    )
    .map_err(|errno| environment("opening directory", anchor, &errno_error(errno)))?;
    let reached = open_regular_at(anchor_fd.as_fd(), relative)
        .map_err(|error| environment(error.operation(), path, &error.into_error()))?;
    let fd = match reached {
        Reached::Regular(fd) => fd,
        Reached::Nothing(unreached) => return Ok(Observed::Nothing(unreached)),
    };
    let mut file = std::fs::File::from(fd);
    let (bytes, content_hash) =
        read_bytes_and_hash(&mut file).map_err(|error| environment("reading", path, &error))?;
    Ok(Observed::Read(ReadAndHash {
        path: path.to_owned(),
        bytes,
        content_hash,
    }))
}

fn errno_error(errno: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(errno.raw_os_error())
}
