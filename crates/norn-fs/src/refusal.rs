//! What the vault filesystem seam refuses, and how it says so.
//!
//! One type for the whole crate, and every refusal is a variant rather than a
//! sentence. The variants are the vocabulary a caller decides against:
//! observed drift is re-planned, a taken name is reported, a symlinked
//! destination is unsupported, and an environmental fault is the machine's
//! problem rather than the plan's. Collapsing any two of those into one shape
//! would make the decision a string match.
//!
//! **A refusal means nothing was published.** Every variant here is reached
//! before the swap or in place of it, so a destination that a refusal names is
//! byte-identical to what it held when the call began. The one thing a refusal
//! may leave behind is a shadow, which is inert (see [`crate::shadow`]).
//!
//! Mapping a refusal onto the wire's structured envelope belongs to whoever
//! serves it. What is here is the engineering fact: which path, and what about
//! it was wrong.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::hash::ContentHash;
use crate::identity::{Identity, PostState};

/// A write, a removal or an acquisition that did not happen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// The bytes at the path are not the bytes the caller observed.
    ///
    /// Both hashes are carried, and `observed` is the whole post-state
    /// identity rather than a hash alone, because that is what a caller needs
    /// to re-compose against: the content it must read again, and the identity
    /// that says whether the file it is looking at is still the same file.
    ///
    /// **`observed` is `None` when nothing is at the path.** A precondition
    /// naming a hash is a statement that a document was there; its absence is
    /// drift with no content to describe, and it is a refusal rather than a
    /// success, because the intent behind the write was derived from bytes that
    /// somebody has since removed.
    ///
    /// A destination that was there when the call began and is gone by the time
    /// the name is confirmed reaches this same arm, and deliberately: it is one
    /// real-world event — the document was removed — and a caller re-planning
    /// against absence should not have to match two variants to hear about it.
    Drifted {
        path: PathBuf,
        /// The hash the caller observed and composed its content against.
        expected: ContentHash,
        /// What is there now, or `None` when nothing is.
        ///
        /// Boxed because this is the widest thing any refusal carries and a
        /// refusal is the returned error of every write: paying for its width in
        /// the success path's return value too is a cost the whole crate would
        /// carry for one arm's diagnostic.
        observed: Option<Box<PostState>>,
    },
    /// The path no longer names the file whose bytes were read.
    ///
    /// The drift family's second member, and the one a hash cannot see. A
    /// foreign writer that *replaced* the document — staged its own copy and
    /// renamed it into place — leaves the held handle pointing at an orphaned
    /// inode whose bytes still hash to what the caller observed. The hash
    /// therefore agrees, and a swap would clobber a document somebody else just
    /// published. The identity comparison is what catches it, and this is what
    /// it reports.
    ///
    /// **A name that resolves to nothing is [`Refusal::Drifted`], not this.**
    /// This variant is the replacement — one file for another — so `current`
    /// always names the file that displaced the one that was read.
    Republished {
        path: PathBuf,
        /// The file the bytes were read from.
        read: Identity,
        /// The file the name means now.
        current: Identity,
    },
    /// A create found the destination already taken.
    ///
    /// Reported by the exclusive create itself rather than by a precheck, so a
    /// destination that springs into existence between a caller's look and this
    /// call is refused with the racer's bytes untouched.
    DestinationExists { path: PathBuf },
    /// The path is a symbolic link, which this crate does not write through.
    ///
    /// **Keyed on being a link, not on where it points.** A link that resolves
    /// would have Norn read a document at one place and publish at another, and
    /// a link that dangles would have a rename replace the link itself. The two
    /// are opposite mistakes and neither is a document write, so the refusal
    /// asks only whether the destination is a link.
    SymlinkDestination { path: PathBuf },
    /// The operating system refused, and this is what it said.
    ///
    /// **Structurally distinct from every precondition refusal above**: those
    /// say the world is not what the caller thought, and this says the machine
    /// could not do the work. `ENOSPC` on a shadow write and `EACCES` on a
    /// destination's directory both arrive here, and so does `EXDEV` from the
    /// swap — a shadow directory on another filesystem, which means the
    /// placement decision has to be made again rather than that the write was
    /// wrong.
    ///
    /// The invariant across every one of them: **the destination is
    /// byte-identical to what it held before the call**, and at most a shadow
    /// is left behind.
    Environment {
        /// What was being attempted, in words that complete "… failed".
        operation: &'static str,
        path: PathBuf,
        /// The kind the standard library classified the error as, so a caller
        /// can separate a missing parent from a denied one without reading the
        /// message.
        kind: std::io::ErrorKind,
        /// The platform's own error number, for the conditions the standard
        /// library has no kind for — `ENOSPC` and `EXDEV` among them.
        raw_os_error: Option<i32>,
        message: String,
    },
    /// The lock file at the path was replaced under every attempt to take it.
    ///
    /// An advisory lock follows the file rather than the name, so holding one
    /// says nothing until the name is known to still resolve to the held
    /// handle. One retry is the ordinary case. A caller that loses that race
    /// [`LOCK_ATTEMPTS`] times in a row is racing something that is removing
    /// the lock file in a loop, and saying so is more use than retrying
    /// forever.
    ///
    /// [`LOCK_ATTEMPTS`]: crate::lock::LOCK_ATTEMPTS
    LockFileReplaced { path: PathBuf, attempts: usize },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Drifted {
                path,
                expected,
                observed: Some(observed),
            } => write!(
                f,
                "{} holds {} and the write was composed against {expected}",
                path.display(),
                observed.content_hash
            ),
            Refusal::Drifted {
                path,
                expected,
                observed: None,
            } => write!(
                f,
                "nothing is at {} and the write was composed against {expected}",
                path.display()
            ),
            Refusal::Republished {
                path,
                read,
                current,
            } => write!(
                f,
                "{} named {read} when it was read and names {current} now",
                path.display()
            ),
            Refusal::DestinationExists { path } => {
                write!(f, "{} already exists", path.display())
            }
            Refusal::SymlinkDestination { path } => write!(
                f,
                "{} is a symbolic link, and a document is written at its own name",
                path.display()
            ),
            Refusal::Environment {
                operation,
                path,
                message,
                ..
            } => write!(f, "{operation} {} failed: {message}", path.display()),
            Refusal::LockFileReplaced { path, attempts } => write!(
                f,
                "{} was replaced on each of {attempts} attempts to lock it",
                path.display()
            ),
        }
    }
}

impl std::error::Error for Refusal {}

/// The refusal for an operating-system error met while `operation` ran against
/// `path`.
pub(crate) fn environment(operation: &'static str, path: &Path, error: &std::io::Error) -> Refusal {
    Refusal::Environment {
        operation,
        path: path.to_path_buf(),
        kind: error.kind(),
        raw_os_error: error.raw_os_error(),
        message: error.to_string(),
    }
}

impl Refusal {
    /// Whether this is the operating system reporting `number`.
    ///
    /// The conditions with no [`std::io::ErrorKind`] of their own are reached
    /// this way: `libc::ENOSPC` for a full disk and `libc::EXDEV` for a rename
    /// across filesystems.
    pub fn is_os_error(&self, number: i32) -> bool {
        matches!(self, Refusal::Environment { raw_os_error: Some(raw), .. } if *raw == number)
    }
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use super::*;

    /// Drift reads as both hashes, so a caller comparing the two by eye sees
    /// which side moved.
    #[test]
    fn drift_names_the_expected_and_the_observed_hash() {
        let expected = ContentHash::of(b"what the caller read");
        let observed = ContentHash::of(b"what is there now");
        let rendered = Refusal::Drifted {
            path: PathBuf::from("/vault/note.md"),
            expected,
            observed: Some(Box::new(PostState {
                content_hash: observed,
                len: 17,
                mtime: std::time::SystemTime::UNIX_EPOCH,
                ino: 3,
                dev: 5,
            })),
        }
        .to_string();
        assert!(rendered.contains(&expected.to_hex()), "{rendered}");
        assert!(rendered.contains(&observed.to_hex()), "{rendered}");
    }

    /// Absence is its own sentence. A diagnostic that named a hash nothing has
    /// would send a reader looking for a file that is not there.
    #[test]
    fn drift_onto_nothing_says_nothing_is_there() {
        let rendered = Refusal::Drifted {
            path: PathBuf::from("/vault/note.md"),
            expected: ContentHash::of(b"gone"),
            observed: None,
        }
        .to_string();
        assert!(rendered.starts_with("nothing is at"), "{rendered}");
    }

    /// The platform's own number is reachable, because the conditions that
    /// matter most here — a full disk, a rename across filesystems — have no
    /// [`ErrorKind`] to match on.
    #[test]
    fn an_environmental_refusal_carries_the_platform_number() {
        let refusal = environment(
            "writing",
            Path::new("/vault/.norn/tmp/shadow"),
            &std::io::Error::from_raw_os_error(libc::ENOSPC),
        );
        assert!(refusal.is_os_error(libc::ENOSPC));
        assert!(!refusal.is_os_error(libc::EXDEV));
        assert!(
            !Refusal::DestinationExists {
                path: PathBuf::from("/vault/note.md")
            }
            .is_os_error(libc::ENOSPC),
            "a precondition refusal answered for the operating system"
        );
    }

    /// The kind survives, so a missing parent is distinguishable from a denied
    /// one without reading prose.
    #[test]
    fn an_environmental_refusal_carries_the_kind() {
        let refusal = environment(
            "opening",
            Path::new("/vault/absent/note.md"),
            &std::io::Error::from(ErrorKind::NotFound),
        );
        assert!(matches!(
            refusal,
            Refusal::Environment {
                kind: ErrorKind::NotFound,
                ..
            }
        ));
    }
}
