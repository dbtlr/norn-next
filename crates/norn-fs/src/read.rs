//! Atomic observation of one file below one anchor directory: its bytes and
//! their hash, or — streamed, holding no bytes — its hash and whether it is
//! UTF-8.

use std::io::{self, Read};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};

use rustix::fs::{Mode, open};

use crate::hash::{ContentHash, hashed_and_checked_from, read_bytes_and_hash};
use crate::open::{
    OpenError, PathOrEnvironment, Reached, Unreached, anchor_flags, open_regular_at_with,
};
use crate::refusal::{Refusal, environment, environment_at};
use crate::walk::faults::WalkFaults;

/// The result of trying to read one document's bytes.
#[derive(Debug)]
pub enum Opened<T> {
    /// The bytes were read and the requested observation is complete.
    Read(T),
    /// No regular file stands at the contained name.
    Nothing,
    /// A folder above the requested file could not be opened.
    UnreadableFolder {
        /// The failed folder, relative to the read's anchor.
        folder: PathBuf,
        /// The operation that failed.
        operation: &'static str,
        /// The operating system's cause.
        cause: io::Error,
    },
    /// This path could not be read; no content is known from this attempt.
    Unreadable {
        /// The operation that failed.
        operation: &'static str,
        /// The operating system's cause, preserved without interpretation.
        cause: io::Error,
    },
}

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
        Observation::Read(read) => Ok(read),
        Observation::Unreadable { operation, cause } => Err(environment(operation, &path, &cause)),
        Observation::UnreadableFolder {
            folder,
            operation,
            cause,
        } => Err(environment(operation, &anchor.join(folder), &cause)),
        Observation::Nothing(unreached) => Err(environment_at(
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
/// A failed file open or read answers [`Opened::Unreadable`], retaining the
/// machine's cause. A folder component's path failure answers
/// [`Opened::UnreadableFolder`] at that folder. Resource failures and an
/// unopenable anchor remain refusals.
pub fn read_optional_and_hash(
    anchor: &Path,
    relative: &Path,
) -> Result<Opened<ReadAndHash>, Refusal> {
    read_optional_with(anchor, relative, &WalkFaults::entry())
}

fn read_optional_with(
    anchor: &Path,
    relative: &Path,
    faults: &WalkFaults,
) -> Result<Opened<ReadAndHash>, Refusal> {
    let path = anchor.join(relative);
    Ok(match observe_with(anchor, relative, &path, faults)? {
        Observation::Read(read) => Opened::Read(read),
        Observation::Nothing(_) => Opened::Nothing,
        Observation::Unreadable { operation, cause } => Opened::Unreadable { operation, cause },
        Observation::UnreadableFolder {
            folder,
            operation,
            cause,
        } => Opened::UnreadableFolder {
            folder,
            operation,
            cause,
        },
    })
}

/// A file's content hash, its length, and whether its bytes are UTF-8, from
/// one streamed read of one held file descriptor that kept none of them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamedHash {
    path: PathBuf,
    content_hash: ContentHash,
    len: u64,
    utf8: bool,
}

impl StreamedHash {
    /// The path that was opened, spelled as the caller named it: the anchor
    /// with the relative name below it.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The content hash of every byte the read streamed.
    pub fn content_hash(&self) -> ContentHash {
        self.content_hash
    }

    /// How many bytes the read streamed.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the read streamed no bytes.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether the bytes streamed are UTF-8: the verdict
    /// [`std::str::from_utf8`] gives the whole buffer.
    pub fn is_utf8(&self) -> bool {
        self.utf8
    }
}

/// Streams the regular file `relative` names below `anchor` once, keeping
/// none of its bytes, or answers that there is no such file to read.
///
/// **The same observation as [`read_optional_and_hash`], less the bytes.**
/// The same contained open, the same answer for a name that reaches no
/// regular file — absent, a link, through a linked folder, a folder or a pipe
/// are all [`Opened::Nothing`] — the same unreadable answer and refusals, and
/// the same count: one [`crate::reads::ReadTally::document_opens`] per file read. What it answers
/// of a file is what the whole read's bytes answer: their hash, their length,
/// and whether they are UTF-8. Peak memory is one 64 KiB chunk whatever the
/// file weighs.
///
/// **For a caller that needs a file's identity and not its text** — a
/// document a plan carries unchanged, whose hash and UTF-8 verdict are all the
/// plan records of it.
///
/// **What a move reads its document by.** Planning and the applier read a
/// document a plan carries byte for byte here, its hash and UTF-8 verdict
/// standing for its body.
pub fn stream_optional_and_hash(
    anchor: &Path,
    relative: &Path,
) -> Result<Opened<StreamedHash>, Refusal> {
    stream_optional_with(anchor, relative, &WalkFaults::entry())
}

fn stream_optional_with(
    anchor: &Path,
    relative: &Path,
    faults: &WalkFaults,
) -> Result<Opened<StreamedHash>, Refusal> {
    let path = anchor.join(relative);
    let mut file = match reach(anchor, relative, &path, faults)? {
        Observation::Read(file) => file,
        Observation::Nothing(_) => return Ok(Opened::Nothing),
        Observation::UnreadableFolder {
            folder,
            operation,
            cause,
        } => {
            return Ok(Opened::UnreadableFolder {
                folder,
                operation,
                cause,
            });
        }
        Observation::Unreadable { operation, cause } => {
            return Ok(Opened::Unreadable { operation, cause });
        }
    };
    let checked = match hashed_and_checked_from(&mut file, &path) {
        Ok(checked) => checked,
        Err(cause) => {
            return Ok(Opened::Unreadable {
                operation: "reading",
                cause: path_failure("reading", &path, cause)?,
            });
        }
    };
    Ok(Opened::Read(StreamedHash {
        path,
        content_hash: checked.hash,
        len: checked.len,
        utf8: checked.utf8,
    }))
}

/// A descriptor failure is unreadable only while the process can still progress.
fn path_failure(
    operation: &'static str,
    path: &Path,
    cause: io::Error,
) -> Result<io::Error, Refusal> {
    match PathOrEnvironment::of(&cause) {
        PathOrEnvironment::Path => Ok(cause),
        PathOrEnvironment::Environment => Err(environment(operation, path, &cause)),
    }
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
        Observation::Read(read) => Ok(Some(read)),
        Observation::Nothing(unreached)
            if unreached.error().kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Observation::Unreadable { operation, cause } => Err(environment(operation, &path, &cause)),
        Observation::UnreadableFolder {
            folder,
            operation,
            cause,
        } => Err(environment(operation, &anchor.join(folder), &cause)),
        Observation::Nothing(unreached) => Err(environment_at(
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
    let mut file = match reach(anchor, relative, &path, &WalkFaults::default())? {
        Observation::Read(file) => file,
        Observation::UnreadableFolder {
            folder,
            operation,
            cause,
        } => return Err(environment(operation, &anchor.join(folder), &cause)),
        Observation::Unreadable { operation, cause } => {
            return Err(environment(operation, &path, &cause));
        }
        Observation::Nothing(unreached)
            if unreached.error().kind() == std::io::ErrorKind::NotFound =>
        {
            return Ok(None);
        }
        Observation::Nothing(unreached) => {
            return Err(environment_at(
                unreached.operation(),
                &path,
                unreached.component(),
                unreached.error(),
            ));
        }
    };
    crate::reads::count_document_read(&path);
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

/// A contained observation retaining the diagnostic for an unreached name.
enum Observation<T> {
    /// The requested descriptor or content.
    Read(T),
    /// A contained name that reaches no regular file.
    Nothing(Unreached),
    /// A folder component failed before reaching the requested file.
    UnreadableFolder {
        folder: PathBuf,
        operation: &'static str,
        cause: io::Error,
    },
    /// A path-specific failure with no content observation.
    Unreadable {
        operation: &'static str,
        cause: io::Error,
    },
}

fn observe(
    anchor: &Path,
    relative: &Path,
    path: &Path,
) -> Result<Observation<ReadAndHash>, Refusal> {
    observe_with(anchor, relative, path, &WalkFaults::default())
}

fn observe_with(
    anchor: &Path,
    relative: &Path,
    path: &Path,
    faults: &WalkFaults,
) -> Result<Observation<ReadAndHash>, Refusal> {
    let mut file = match reach(anchor, relative, path, faults)? {
        Observation::Read(file) => file,
        Observation::Nothing(unreached) => return Ok(Observation::Nothing(unreached)),
        Observation::UnreadableFolder {
            folder,
            operation,
            cause,
        } => {
            return Ok(Observation::UnreadableFolder {
                folder,
                operation,
                cause,
            });
        }
        Observation::Unreadable { operation, cause } => {
            return Ok(Observation::Unreadable { operation, cause });
        }
    };
    let (bytes, content_hash) = match read_bytes_and_hash(&mut file, path) {
        Ok(read) => read,
        Err(cause) => {
            return Ok(Observation::Unreadable {
                operation: "reading",
                cause: path_failure("reading", path, cause)?,
            });
        }
    };
    Ok(Observation::Read(ReadAndHash {
        path: path.to_owned(),
        bytes,
        content_hash,
    }))
}

/// The contained open; anchor and resource failures remain refusals.
#[allow(clippy::disallowed_types)] // norn-fs owns file handles.
fn reach(
    anchor: &Path,
    relative: &Path,
    path: &Path,
    faults: &WalkFaults,
) -> Result<Observation<std::fs::File>, Refusal> {
    let anchor_fd = open(anchor, anchor_flags(), Mode::empty())
        .map_err(|errno| environment("opening directory", anchor, &errno_error(errno)))?;
    let reached = match open_regular_at_with(anchor_fd.as_fd(), relative, path, faults) {
        Ok(reached) => reached,
        Err(OpenError::Folder { at: folder, errno }) => {
            return Ok(Observation::UnreadableFolder {
                folder,
                operation: "opening",
                cause: errno_error(errno),
            });
        }
        Err(error @ OpenError::Path { .. }) => {
            return Ok(Observation::Unreadable {
                operation: error.operation(),
                cause: error.into_error(),
            });
        }
        Err(error) => {
            let (operation, component) = (error.operation(), error.component().to_owned());
            return Err(environment_at(
                operation,
                path,
                &component,
                &error.into_error(),
            ));
        }
    };
    Ok(match reached {
        Reached::Regular(fd) => Observation::Read(std::fs::File::from(fd)),
        Reached::Nothing(unreached) => Observation::Nothing(unreached),
    })
}

fn errno_error(errno: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(errno.raw_os_error())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::reads::ReadWindow;
    use crate::scratch::Scratch;
    use crate::walk::faults::{Answer, Stage};

    #[test]
    fn read_optional_and_hash_answers_unreadable() {
        let scratch = Scratch::new("optional-open-denied");
        scratch.place("note.md", b"body");
        let faults = WalkFaults::at(&[(Stage::Open, Answer::Denied)]);
        let answer = read_optional_with(&scratch.at(""), Path::new("note.md"), &faults);
        assert!(
            matches!(answer, Ok(Opened::Unreadable { operation: "opening", cause })
            if cause.raw_os_error() == Some(libc::EACCES))
        );
        let read = crate::scratch::read(
            read_optional_with(&scratch.at(""), Path::new("note.md"), &faults).unwrap(),
        );
        assert_eq!(
            read.bytes(),
            b"body",
            "the next attempt reads after the condition ends"
        );
    }

    #[test]
    fn stream_optional_and_hash_answers_unreadable() {
        let scratch = Scratch::new("stream-open-failing");
        scratch.place("note.md", b"body");
        let answer = stream_optional_with(
            &scratch.at(""),
            Path::new("note.md"),
            &WalkFaults::at(&[(Stage::Open, Answer::Failing)]),
        );
        assert!(
            matches!(answer, Ok(Opened::Unreadable { operation: "opening", cause })
            if cause.raw_os_error() == Some(libc::EIO))
        );
    }

    #[test]
    fn optional_hash_readers_refuse_descriptor_exhaustion() {
        let scratch = Scratch::new("optional-open-exhausted");
        scratch.place("note.md", b"body");
        let whole = read_optional_with(
            &scratch.at(""),
            Path::new("note.md"),
            &WalkFaults::at(&[(Stage::Open, Answer::Exhausted)]),
        )
        .unwrap_err();
        let streamed = stream_optional_with(
            &scratch.at(""),
            Path::new("note.md"),
            &WalkFaults::at(&[(Stage::Open, Answer::Exhausted)]),
        )
        .unwrap_err();
        for refusal in [whole, streamed] {
            assert!(matches!(
                refusal,
                Refusal::Environment {
                    raw_os_error: Some(libc::EMFILE),
                    ..
                }
            ));
        }
    }

    /// **A contained read counts its reads, not its opens.** Each of the four
    /// readers — the streamed one among them — reads the file once and is one
    /// `document_opens`; a name holding no file is read by none of them and
    /// counts nothing. Under
    /// `induced-failure` each read names the file it read, spelled as the
    /// anchor joined with the name below it.
    #[test]
    fn a_contained_read_counts_each_read_of_its_file() {
        let scratch = Scratch::new("contained-reads");
        scratch.directory("vault/notes");
        let note = scratch.place("notes/note.md", b"body");
        let anchor = scratch.at("");
        #[cfg(feature = "induced-failure")]
        let _serial = crate::reads::recording_cases();
        #[cfg(feature = "induced-failure")]
        let _recording = crate::reads::record_files();

        let window = ReadWindow::open();
        read_and_hash(&anchor, Path::new("notes/note.md")).expect("a read");
        read_optional_and_hash(&anchor, Path::new("notes/note.md")).expect("a read");
        read_if_present_bounded(&anchor, Path::new("notes/note.md"), 64).expect("a read");
        stream_optional_and_hash(&anchor, Path::new("notes/note.md")).expect("a read");
        read_optional_and_hash(&anchor, Path::new("notes/absent.md")).expect("an answer");
        stream_optional_and_hash(&anchor, Path::new("notes/absent.md")).expect("an answer");
        #[cfg(feature = "induced-failure")]
        {
            let (tally, files) = window.finish_with_files();
            assert_eq!(tally.document_opens, 4, "{tally:?}");
            let read = crate::reads::FileRead {
                act: crate::reads::ReadAct::Document,
                path: anchor.join("notes/note.md"),
            };
            assert_eq!(files, vec![read.clone(), read.clone(), read.clone(), read]);
            assert_eq!(note, anchor.join("notes/note.md"));
        }
        #[cfg(not(feature = "induced-failure"))]
        {
            let _ = note;
            assert_eq!(window.finish().document_opens, 4);
        }
    }
}
