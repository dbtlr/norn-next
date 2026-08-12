//! Whether a reported invalidation reaches a path.
//!
//! A watcher reports invalidation as roots: a vault-relative path whose own
//! entry and every descendant of it may have changed. Two suites read those
//! reports back and ask the same question of them — the platform-watcher
//! cases in `norn-fs`, which ask whether the backend delivered an event for a
//! path, and the host's absorbing waits, which ask whether a settled batch
//! invalidated the document a case edited. The question is at-or-above
//! containment either way, so [`covers`] is where it is answered, and a suite
//! asking it spells no containment of its own.
//!
//! What this module is not is the vault's own containment answer. It judges
//! reports a harness collected, both sides of the comparison produced by this
//! process in one run; `norn-fs` owns what a *configured* root contains, over
//! observed paths on a volume whose case behavior it probed, and exports that
//! through one type so a consumer cannot spell a second answer to it.

use std::path::Path;

/// Whether the invalidation root `root` is at or above `path` — the path
/// itself, or a directory containing it.
///
/// Containment is judged component by component: `notes` covers
/// `notes/today.md`, and covers neither `notes-archive/today.md` nor
/// `notesbook`, while `notes/note` covers nothing under `notes/note.md`. A
/// byte prefix answers yes to the last two, and an equality test answers no to
/// the first — the two ways a suite gets this wrong, which is why the answer
/// is one function rather than a line each call site writes.
///
/// **Both sides are compared exactly, case included.** A root reaches this
/// function from a `norn-fs` batch, so it is already normalized: relative,
/// non-empty, and carrying no `.` or `..` component. `path` is the spelling
/// the case itself wrote to disk, and the backend reports the spelling that is
/// on disk — so the two agree on case by construction, and nothing here folds
/// it. A case that makes them disagree is asserting about a volume's case
/// behavior, which is `norn-fs`'s subject and not a harness comparison's.
pub fn covers(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_root_covers_itself_and_everything_beneath_it() {
        assert!(covers(Path::new("notes"), Path::new("notes")));
        assert!(covers(Path::new("notes"), Path::new("notes/today.md")));
        assert!(covers(
            Path::new("notes"),
            Path::new("notes/2026/08/today.md")
        ));
    }

    #[test]
    fn a_root_that_is_only_a_byte_prefix_covers_nothing() {
        assert!(!covers(Path::new("notes"), Path::new("notesbook/today.md")));
        assert!(!covers(
            Path::new("notes"),
            Path::new("notes-archive/today.md")
        ));
        assert!(!covers(Path::new("notes/note"), Path::new("notes/note.md")));
    }

    #[test]
    fn a_root_beneath_the_path_covers_it_no_more_than_a_sibling_does() {
        assert!(!covers(Path::new("notes/today.md"), Path::new("notes")));
        assert!(!covers(
            Path::new("notes/today.md"),
            Path::new("notes/yesterday.md")
        ));
    }
}
