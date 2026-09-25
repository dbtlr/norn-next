//! Atomic observation of one file below one anchor directory.

use std::io::{self, Read};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};

use rustix::fs::{Mode, open};

use crate::hash::{ContentHash, read_bytes_and_hash};
use crate::open::{Reached, Unreached, anchor_flags, open_regular_at};
use crate::refusal::{Refusal, environment, environment_at};

/// What kind of filesystem object a watcher invalidation root names now.
///
/// [`crate::Vault::reach`] is what reads one, from a vault's own root
/// descriptor down: a name is reached through the tree the vault walked, and a
/// spelling only a folding volume resolves stands at nothing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathKind {
    Missing,
    RegularFile,
    Directory,
    Other,
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
/// The read is contained (`open`'s discipline): no component of `relative` is followed
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
        Observed::Nothing(unreached) => Err(environment_at(
            unreached.operation(),
            &path,
            unreached.component(),
            unreached.error(),
        )),
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

/// Reads a regular file, or returns `None` when its contained path is missing.
///
/// Only a missing path produces `None`. A symbolic link, directory, pipe,
/// socket, or other non-file is a refusal. Use this for an optional control
/// file whose invalid shape must stay visible to its caller.
pub fn read_if_present_and_hash(
    anchor: &Path,
    relative: &Path,
) -> Result<Option<ReadAndHash>, Refusal> {
    let path = anchor.join(relative);
    match observe(anchor, relative, &path)? {
        Observed::Read(read) => Ok(Some(read)),
        Observed::Nothing(unreached)
            if unreached.error().kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Observed::Nothing(unreached) => Err(environment_at(
            unreached.operation(),
            &path,
            unreached.component(),
            unreached.error(),
        )),
    }
}

/// What a read of at most a bound's bytes of one file found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Bounded {
    /// The whole file, which is no longer than the bound.
    Whole(Vec<u8>),
    /// The file holds more than the bound, and nothing past it was read.
    Longer,
}

/// Reads the regular file `relative` names below `anchor` where it holds at
/// most `bound` bytes, answers [`Bounded::Longer`] where it holds more, or
/// answers that there is no such file.
///
/// The same containment and the same stance on an unreached name as
/// [`read_if_present_and_hash`]: only a missing path is `None`, and a link, a
/// directory, a pipe or a socket is a refusal, reached without waiting on a
/// pipe's writer. At most `bound` bytes and one more are read, and nothing is
/// hashed.
pub(crate) fn read_if_present_bounded(
    anchor: &Path,
    relative: &Path,
    bound: usize,
) -> Result<Option<Bounded>, Refusal> {
    let path = anchor.join(relative);
    let mut file = match reach(anchor, relative, &path)? {
        Ok(file) => file,
        Err(unreached) if unreached.error().kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(unreached) => {
            return Err(environment_at(
                unreached.operation(),
                &path,
                unreached.component(),
                unreached.error(),
            ));
        }
    };
    let mut bytes = Vec::new();
    let limit = u64::try_from(bound).unwrap_or(u64::MAX).saturating_add(1);
    (&mut file)
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|error| environment("reading", &path, &error))?;
    Ok(Some(if bytes.len() > bound {
        Bounded::Longer
    } else {
        Bounded::Whole(bytes)
    }))
}

/// One observation, before either caller decides what an unreached name means.
enum Observed {
    Read(ReadAndHash),
    Nothing(Unreached),
}

fn observe(anchor: &Path, relative: &Path, path: &Path) -> Result<Observed, Refusal> {
    let mut file = match reach(anchor, relative, path)? {
        Ok(file) => file,
        Err(unreached) => return Ok(Observed::Nothing(unreached)),
    };
    let (bytes, content_hash) =
        read_bytes_and_hash(&mut file).map_err(|error| environment("reading", path, &error))?;
    Ok(Observed::Read(ReadAndHash {
        path: path.to_owned(),
        bytes,
        content_hash,
    }))
}

/// The regular file `relative` names below `anchor`, opened through the
/// contained open, or the fact about the name that stopped the descent.
#[allow(clippy::disallowed_types)] // norn-fs owns file handles.
fn reach(
    anchor: &Path,
    relative: &Path,
    path: &Path,
) -> Result<Result<std::fs::File, Unreached>, Refusal> {
    let anchor_fd = open(anchor, anchor_flags(), Mode::empty())
        .map_err(|errno| environment("opening directory", anchor, &errno_error(errno)))?;
    let reached = open_regular_at(anchor_fd.as_fd(), relative).map_err(|error| {
        let (operation, component) = (error.operation(), error.component().to_owned());
        environment_at(operation, path, &component, &error.into_error())
    })?;
    Ok(match reached {
        Reached::Regular(fd) => Ok(std::fs::File::from(fd)),
        Reached::Nothing(unreached) => Err(unreached),
    })
}

fn errno_error(errno: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(errno.raw_os_error())
}
