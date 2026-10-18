#![forbid(unsafe_code)]

//! The spelling a folder stores an entry under, asked of the entry itself.
//!
//! On a root that folds, a stat answers at every spelling the volume
//! resolves, so which spelling is the entry is a question the folder's
//! listing answers by reading siblings until it finds the name. A volume that
//! keeps each entry's own name answers it from a descriptor of the entry
//! instead: one query, whatever the folder's width. This module asks that
//! query where the folder's volume answers it, and answers `None` everywhere
//! else, so the listing stays the authority on every other folding volume.

use std::ffi::{OsStr, OsString};
use std::os::fd::BorrowedFd;

/// The spelling `folder` stores the entry at `name` under, asked of a
/// descriptor opened on it; `None` where `folder`'s volume has no such answer
/// or the entry cannot be opened.
///
/// **Ask it only of an entry a stat read as a directory or a regular file.**
/// The open is a contained read's — no link is followed, and a FIFO swapped in
/// since the stat does not hold the call — but opening a special file at all
/// is an act on it, which the listing this replaces never takes.
///
/// **The descriptor is opened only where the answer can be had.** A folder on
/// another volume opens nothing, so a caller pays the open only where it
/// replaces a listing. An entry that cannot be opened — gone, a link, or not
/// readable — answers `None`, and so does a file with several links, which
/// has no one name; the caller reads the listing, which answers for either
/// without opening it.
pub(crate) fn stored_name_at(folder: BorrowedFd<'_>, name: &OsStr) -> Option<OsString> {
    platform::stored_name_at(folder, name)
}

/// The spelling `folder` stores the entry `entry` is open on under, where
/// `folder`'s volume answers it; `None` where it does not.
///
/// `entry` is a descriptor the caller opened at a name in `folder`, so the
/// answer is the stored spelling of that name's entry, whichever spelling
/// opened it. A file with several links answers `None`.
pub(crate) fn stored_name_of(folder: BorrowedFd<'_>, entry: BorrowedFd<'_>) -> Option<OsString> {
    platform::stored_name_of(folder, entry)
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{OsStr, OsString};
    use std::os::fd::{AsFd, BorrowedFd};
    use std::os::unix::ffi::OsStrExt;

    use rustix::fs::{FileType, Mode, fstat, fstatfs, getpath, openat};

    use crate::open::regular_flags;

    pub(super) fn stored_name_at(folder: BorrowedFd<'_>, name: &OsStr) -> Option<OsString> {
        if !answers_stored_names(folder) {
            return None;
        }
        let entry = openat(folder, name, regular_flags(), Mode::empty()).ok()?;
        query(entry.as_fd())
    }

    pub(super) fn stored_name_of(
        folder: BorrowedFd<'_>,
        entry: BorrowedFd<'_>,
    ) -> Option<OsString> {
        if !answers_stored_names(folder) {
            return None;
        }
        query(entry)
    }

    /// Whether the volume `folder` is on names a descriptor's path from the
    /// entry's own record.
    ///
    /// **APFS and HFS+ do**: the path `F_GETPATH` hands back ends in the
    /// spelling the folder lists, whichever spelling opened the entry, the new
    /// one after a case-only rename, and the covered folder's own name at a
    /// mount point. The folder is asked, not the entry, because a name is
    /// stored by the volume of the folder that holds it. Other volumes —
    /// exFAT, FAT, SMB, NFS — are not asked, since a path a volume builds from
    /// the spelling a lookup used would read back whatever the caller spelled.
    fn answers_stored_names(folder: BorrowedFd<'_>) -> bool {
        let Ok(volume) = fstatfs(folder) else {
            return false;
        };
        let kind: Vec<u8> = volume
            .f_fstypename
            .iter()
            .take_while(|byte| **byte != 0)
            .map(|byte| *byte as u8)
            .collect();
        matches!(kind.as_slice(), b"apfs" | b"hfs")
    }

    /// The last name of `entry`'s path, as its volume names it; `None` for a
    /// file with several links.
    ///
    /// **A file with several links has no one name to answer.** The volume
    /// names it by whichever of its links it looked up last, so a lookup of
    /// another link — in this folder or another, by any process — between
    /// the open and the query would answer that link's spelling. A file with
    /// one link has one name, and that is the answer. A directory is not
    /// asked its link count, which does not count its names; it has one name
    /// except as a hard-linked directory, which HFS+ allows and only Time
    /// Machine makes, and that is outside this answer.
    fn query(entry: BorrowedFd<'_>) -> Option<OsString> {
        let stat = fstat(entry).ok()?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::Directory && stat.st_nlink != 1 {
            return None;
        }
        crate::reads::count_name_query();
        let path = getpath(entry).ok()?;
        let name = path.as_bytes().rsplit(|byte| *byte == b'/').next()?;
        (!name.is_empty()).then(|| OsStr::from_bytes(name).to_owned())
    }
}

/// No other platform's volumes answer a stored name through a descriptor,
/// so every folding root there reads its listing.
#[cfg(not(target_os = "macos"))]
mod platform {
    use std::ffi::{OsStr, OsString};
    use std::os::fd::BorrowedFd;

    pub(super) fn stored_name_at(_folder: BorrowedFd<'_>, _name: &OsStr) -> Option<OsString> {
        None
    }

    pub(super) fn stored_name_of(
        _folder: BorrowedFd<'_>,
        _entry: BorrowedFd<'_>,
    ) -> Option<OsString> {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::os::fd::AsFd;

    use norn_testkit::churn::{folding, runs_where_the_volume_folds};
    use rustix::fs::{Mode, open, openat};

    use super::*;
    use crate::open::{anchor_flags, regular_flags};
    use crate::scratch::Scratch;

    /// Whether this host's volumes answer a stored name: macOS, whose suite
    /// runs on APFS.
    fn answers_here() -> bool {
        cfg!(target_os = "macos")
    }

    /// Whether the spelling cases bind here: an answering volume that folds,
    /// where a stored name is a different question from the name asked. A
    /// macOS scratch volume that does not fold fails the case.
    #[track_caller]
    fn folds_and_answers(scratch: &Scratch, case: &str) -> bool {
        answers_here()
            && runs_where_the_volume_folds(
                folding(&scratch.at("")).expect("the scratch volume"),
                case,
            )
    }

    fn stored(scratch: &Scratch, folder: &str, name: &str) -> Option<OsString> {
        let folder = open(scratch.at(folder), anchor_flags(), Mode::empty()).expect("the folder");
        stored_name_at(folder.as_fd(), OsStr::new(name))
    }

    /// **A folding macOS volume answers the spelling its folder lists**,
    /// whichever spelling asked: another ASCII case, the composed form of a
    /// name stored decomposed, another Unicode case, and after a case-only
    /// rename the new spelling rather than the one the entry was made at.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: respelling the arranged entries.
    fn a_folding_volume_answers_the_listed_spelling_whatever_asks() {
        let scratch = Scratch::new("stored-name-spellings");
        if !folds_and_answers(&scratch, "stored names answer the listed spelling") {
            return;
        }
        scratch.place("Note.md", b"body");
        scratch.place("re\u{301}sume\u{301}.md", b"body");
        scratch.place("\u{c4}pfel.md", b"body");
        scratch.directory("vault/Folder");
        for (asked, listed) in [
            ("note.md", "Note.md"),
            ("NOTE.MD", "Note.md"),
            ("Note.md", "Note.md"),
            ("r\u{e9}sum\u{e9}.md", "re\u{301}sume\u{301}.md"),
            ("\u{e4}pfel.md", "\u{c4}pfel.md"),
            ("FOLDER", "Folder"),
        ] {
            assert_eq!(
                stored(&scratch, "", asked).as_deref(),
                Some(OsStr::new(listed)),
                "{asked}"
            );
        }

        std::fs::rename(scratch.at("Note.md"), scratch.at("note.md")).expect("the respell");
        std::fs::rename(scratch.at("Folder"), scratch.at("fOLDER")).expect("the respell");
        assert_eq!(
            stored(&scratch, "", "Note.md").as_deref(),
            Some(OsStr::new("note.md"))
        );
        assert_eq!(
            stored(&scratch, "", "Folder").as_deref(),
            Some(OsStr::new("fOLDER"))
        );
    }

    /// **A file with several links has no stored-name answer**, so its caller
    /// reads the listing. The volume names such a file by whichever of its
    /// links it looked up last, which need not be the link the descriptor
    /// was opened through: after another folder's link is looked up, a
    /// descriptor opened here would answer that folder's spelling.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: linking and stating the arranged file.
    fn a_file_with_several_links_has_no_stored_name() {
        let scratch = Scratch::new("stored-name-links");
        scratch.directory("vault/x");
        scratch.directory("vault/y");
        scratch.place("x/Note.md", b"body");
        std::fs::hard_link(scratch.at("x/Note.md"), scratch.at("y/NOTE.md")).expect("a link");
        let folder = open(scratch.at("x"), anchor_flags(), Mode::empty()).expect("the folder");
        let entry =
            openat(folder.as_fd(), "Note.md", regular_flags(), Mode::empty()).expect("the entry");
        std::fs::symlink_metadata(scratch.at("y/NOTE.md")).expect("the other link");
        assert_eq!(stored_name_of(folder.as_fd(), entry.as_fd()), None);
        assert_eq!(stored(&scratch, "x", "Note.md"), None);
        assert_eq!(stored(&scratch, "y", "NOTE.md"), None);
    }

    /// **An entry the open does not reach has no answer**, so its caller
    /// reads the listing: a missing name, and a symbolic link, which the open
    /// does not follow.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging a link.
    fn an_entry_the_open_does_not_reach_has_no_stored_name() {
        let scratch = Scratch::new("stored-name-unopened");
        scratch.place("note.md", b"body");
        std::os::unix::fs::symlink("note.md", scratch.at("link.md")).expect("a link");
        for asked in ["gone.md", "link.md"] {
            assert_eq!(stored(&scratch, "", asked), None, "{asked}");
        }
    }

    /// **A query is counted once per descriptor asked**, on the hosts whose
    /// volumes answer one, and nowhere else.
    #[test]
    fn a_stored_name_query_counts_once_per_descriptor() {
        let scratch = Scratch::new("stored-name-count");
        scratch.place("note.md", b"body");
        let folder = open(scratch.at(""), anchor_flags(), Mode::empty()).expect("the folder");
        let entry =
            openat(folder.as_fd(), "note.md", regular_flags(), Mode::empty()).expect("the entry");
        let window = crate::reads::ReadWindow::open();
        let answer = stored_name_of(folder.as_fd(), entry.as_fd());
        let tally = window.finish();
        if answers_here() {
            assert_eq!(answer.as_deref(), Some(OsStr::new("note.md")));
            assert_eq!(tally.name_queries, 1);
        } else {
            assert_eq!(answer, None);
            assert_eq!(tally.name_queries, 0);
        }
        assert_eq!(tally.walk_dirents + tally.write_dirents, 0);
    }
}
