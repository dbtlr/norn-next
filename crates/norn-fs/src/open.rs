//! The one open every file read goes through.
//!
//! A read is anchored at a directory and reaches a name below it one component
//! at a time. Every component is opened with `O_NOFOLLOW`, so a symbolic link
//! anywhere in the path ends the descent instead of redirecting it: a link is a
//! fact about a name, never a traversal edge. A single multi-component open
//! would resolve the intermediate names in the kernel, where `O_NOFOLLOW` binds
//! only the last of them, and a name below the anchor could then be read
//! through a link that leaves it.
//!
//! The last component is opened with `O_NONBLOCK` as well, and its type is
//! proven through the returned descriptor rather than by a stat of the name
//! beforehand. Both halves are load-bearing: a FIFO at a document's name holds
//! an ordinary `open` until somebody writes to the pipe — which is a worker
//! parked forever — and a name checked before it is opened is a name that can
//! change in between. `O_NONBLOCK` is inert for reads of a regular file on this
//! crate's platform floor, so the descriptor handed back reads like any other.
//!
//! What is *not* here is a policy. Reaching no regular file is reported as the
//! fact about the name that stopped the descent, and each caller decides
//! whether that fact is an absence to converge on or a refusal to report.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Component, Path};

use rustix::fs::{AtFlags, FileType, Mode, OFlags, fstat, openat, statat};
use rustix::io::Errno;

/// The flags every directory a contained descent passes through is opened with.
pub(crate) fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY | OFlags::NOFOLLOW
}

/// The flags the anchor a contained descent starts from is opened with.
///
/// `O_NOFOLLOW` is absent here, and its absence is a rule rather than an
/// oversight: the anchor **is** the boundary, so it is resolved exactly as the
/// caller spelled it, while every name below it is a name inside the boundary
/// and is reached through no link at all. The lock file's open splits the same
/// way — the directory holding it is resolved as configured and only the lock's
/// own name refuses a link.
///
/// The vault walk opens its root with [`directory_flags`] instead, because a
/// walk states what a whole tree holds and a root that is itself a link is a
/// tree it declines to enumerate. A read anchored at that same directory
/// answers a narrower question and does not re-derive the name it was handed.
pub(crate) fn anchor_flags() -> OFlags {
    OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY
}

/// The flags the last component of a contained descent is opened with.
fn regular_flags() -> OFlags {
    OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK
}

/// What a contained open reached.
pub(crate) enum Reached {
    /// A descriptor whose file is a regular file, proven through that same
    /// descriptor.
    Regular(OwnedFd),
    /// No regular file, and the fact about a name that says so.
    Nothing(Unreached),
}

/// The fact about a name that stopped a contained open short of a regular file.
///
/// Every one of these is a statement about what is on the filesystem, never
/// about whether the machine could act: a denied directory, an exhausted
/// descriptor table and a failing device are the error half of the same result
/// and never arrive here.
///
/// The component is carried with the fact because a path has more than one name
/// in it and only one of them stopped the descent. A caller that reported the
/// whole path and the condition would leave an operator to work out which name
/// the sentence is about — and the two-name default schema path is where that
/// question is asked most.
#[derive(Debug)]
pub(crate) struct Unreached {
    operation: &'static str,
    component: OsString,
    error: io::Error,
}

impl Unreached {
    /// Nothing is at one of the names in the path.
    fn missing(component: &OsStr, errno: Errno) -> Self {
        Self {
            operation: "opening",
            component: component.to_owned(),
            error: io::Error::from_raw_os_error(errno.raw_os_error()),
        }
    }

    /// One of the names in the path is a symbolic link.
    fn symbolic_link(component: &OsStr) -> Self {
        Self {
            operation: "opening",
            component: component.to_owned(),
            error: io::Error::new(
                io::ErrorKind::InvalidInput,
                "a name in the path is a symbolic link",
            ),
        }
    }

    /// One of the names before the last is not a directory.
    fn not_a_directory(component: &OsStr) -> Self {
        Self {
            operation: "opening",
            component: component.to_owned(),
            error: io::Error::new(
                io::ErrorKind::InvalidInput,
                "a name in the path is not a directory",
            ),
        }
    }

    /// The last name is a directory, a device, a socket or a pipe.
    fn not_regular(component: &OsStr) -> Self {
        Self {
            operation: "reading",
            component: component.to_owned(),
            error: io::Error::new(
                io::ErrorKind::InvalidData,
                "the name does not identify a regular file",
            ),
        }
    }

    /// What was being attempted, in words that complete "… failed".
    pub(crate) fn operation(&self) -> &'static str {
        self.operation
    }

    /// The name the descent stopped at, which is the one the fact is about.
    pub(crate) fn component(&self) -> &OsStr {
        &self.component
    }

    /// The fact stated as the error a refusal carries.
    pub(crate) fn error(&self) -> &io::Error {
        &self.error
    }
}

/// Opens the regular file `relative` names below the already-open `anchor`.
///
/// `relative` is resolved component by component from `anchor`, and nothing
/// above `anchor` is consulted: the descent can only reach names the anchor
/// directory contains, transitively, through real directories.
///
/// Only ordinary names descend. A component that names a filesystem root, a
/// platform prefix or a parent directory is refused before any of it is opened,
/// because none of the three is a name below the anchor: `openat` resolves an
/// absolute name from the filesystem root and ignores the descriptor it was
/// handed, and `..` walks out of the anchor with `O_NOFOLLOW` set and no error
/// to report. Refusing them is what makes the containment above a property of
/// this function rather than of whoever calls it.
pub(crate) fn open_regular_at(
    anchor: BorrowedFd<'_>,
    relative: &Path,
) -> Result<Reached, OpenError> {
    let mut components = relative.components().peekable();
    let mut descended: Option<OwnedFd> = None;
    while let Some(component) = components.next() {
        let name = component.as_os_str();
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) | Component::ParentDir => {
                return Err(OpenError::Uncontained {
                    component: name.to_owned(),
                });
            }
        }
        let last = components.peek().is_none();
        let opened = {
            let parent = descended.as_ref().map_or(anchor, AsFd::as_fd);
            let flags = if last {
                regular_flags()
            } else {
                directory_flags()
            };
            openat(parent, name, flags, Mode::empty())
        };
        let fd = match opened {
            Ok(fd) => fd,
            Err(errno) => {
                let parent = descended.as_ref().map_or(anchor, AsFd::as_fd);
                return classify(parent, name, errno);
            }
        };
        if !last {
            descended = Some(fd);
            continue;
        }
        crate::reads::count_stat();
        let stat = fstat(&fd).map_err(|errno| OpenError::Machine {
            errno,
            component: name.to_owned(),
        })?;
        let kind = FileType::from_raw_mode(stat.st_mode as _);
        return Ok(match kind {
            FileType::RegularFile => {
                crate::reads::count_document_open();
                Reached::Regular(fd)
            }
            _ => Reached::Nothing(Unreached::not_regular(name)),
        });
    }
    // An empty relative path names the anchor directory itself, which is not a
    // regular file and is not opened again to say so. The name it stopped at is
    // the anchor, spelled the way an empty path spells it.
    Ok(Reached::Nothing(Unreached::not_regular(OsStr::new("."))))
}

/// What stops an open that is not a fact about a name on the filesystem.
///
/// Each arm carries the component it is about, for the reason [`Unreached`]
/// does: a denied directory in the middle of a path is a different thing to fix
/// than a denied document, and the whole path says neither.
#[derive(Debug)]
pub(crate) enum OpenError {
    /// The machine refused: a denied directory, a full descriptor table, a
    /// failing device.
    Machine { errno: Errno, component: OsString },
    /// The relative path is not a name below the anchor at all.
    Uncontained { component: OsString },
}

impl OpenError {
    /// What was being attempted, in words that complete "… failed".
    pub(crate) fn operation(&self) -> &'static str {
        "opening"
    }

    /// The name the open stopped at, which is the one the failure is about.
    pub(crate) fn component(&self) -> &OsStr {
        match self {
            Self::Machine { component, .. } | Self::Uncontained { component } => component,
        }
    }

    /// The failure stated as the error a refusal carries.
    pub(crate) fn into_error(self) -> io::Error {
        match self {
            Self::Machine { errno, .. } => io::Error::from_raw_os_error(errno.raw_os_error()),
            Self::Uncontained { .. } => io::Error::new(
                io::ErrorKind::InvalidInput,
                "the path leaves the directory it is read from",
            ),
        }
    }
}

/// Separates the facts about names from the machine's own failures.
///
/// A refused component is asked about by name, because which error `O_NOFOLLOW`
/// reports for a link differs between the supported platforms: one says the
/// name is a link and the other says it is not a directory. The stat costs
/// nothing in the common case — it happens only where an open already failed —
/// and it is what lets the refusal name the condition an operator has to fix.
///
/// The kinds an open declines to give a descriptor for are facts too. A socket
/// cannot be opened at all — `ENXIO` on one platform, `EOPNOTSUPP` on the other
/// — and a name longer than the filesystem holds names is a name no file is
/// ever at. Both say the same thing the type check after a successful open
/// says: no regular file is there. Reporting them as the machine's failure
/// would make an optional read refuse where its own contract promises an
/// answer, and the racing case is ordinary — a document replaced by a socket
/// between a stat and this open.
#[allow(clippy::disallowed_methods)] // norn-fs owns vault stat.
fn classify(parent: BorrowedFd<'_>, name: &OsStr, errno: Errno) -> Result<Reached, OpenError> {
    match errno {
        Errno::NOENT | Errno::NAMETOOLONG => Ok(Reached::Nothing(Unreached::missing(name, errno))),
        Errno::LOOP => Ok(Reached::Nothing(Unreached::symbolic_link(name))),
        Errno::NXIO | Errno::OPNOTSUPP => Ok(Reached::Nothing(Unreached::not_regular(name))),
        Errno::NOTDIR => Ok(Reached::Nothing({
            crate::reads::count_stat();
            match statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) if FileType::from_raw_mode(stat.st_mode as _) == FileType::Symlink => {
                    Unreached::symbolic_link(name)
                }
                _ => Unreached::not_a_directory(name),
            }
        })),
        errno => Err(OpenError::Machine {
            errno,
            component: name.to_owned(),
        }),
    }
}
