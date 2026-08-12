//! Whether a reported invalidation reaches a path.
//!
//! A watcher reports invalidation as roots: a vault-relative path whose own
//! entry and every descendant of it may have changed. Two suites read those
//! reports back and ask the same question of them — the platform-watcher
//! cases in `norn-fs`, which ask whether the backend delivered an event for a
//! path, and the host's absorbing waits, which ask whether a settled batch
//! invalidated the document a case edited. The question is at-or-above
//! containment either way, so [`at_or_above`] is where it is answered, and a
//! suite asking it spells no containment of its own.
//!
//! At-or-above is the narrow half of what a suite usually wants. "Some
//! invalidation reaches this path" also admits a vault-wide rescan, which
//! names no root at all; that wider question stays with the caller, which is
//! the side holding the rescans, and the name here says which half this is.
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
/// Containment is judged component by component: `notes` is at or above
/// `notes/today.md`, and above neither `notes-archive/today.md` nor
/// `notesbook`, while `notes/note` is above nothing under `notes/note.md`. A
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
///
/// **An empty root is above nothing.** `Path::starts_with` takes it as a
/// prefix of every path, so a caller that lost its root — parsing one back out
/// of a rendering, say — would get a condition that holds on the first report
/// absorbed and an assertion that proves nothing. The answer for it is no, so
/// that caller's wait expires and says so.
pub fn at_or_above(root: &Path, path: &Path) -> bool {
    !root.as_os_str().is_empty() && path.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_root_is_at_or_above_itself_and_everything_beneath_it() {
        assert!(at_or_above(Path::new("notes"), Path::new("notes")));
        assert!(at_or_above(Path::new("notes"), Path::new("notes/today.md")));
        assert!(at_or_above(
            Path::new("notes"),
            Path::new("notes/2026/08/today.md")
        ));
    }

    #[test]
    fn a_root_that_is_only_a_byte_prefix_is_above_nothing() {
        assert!(!at_or_above(
            Path::new("notes"),
            Path::new("notesbook/today.md")
        ));
        assert!(!at_or_above(
            Path::new("notes"),
            Path::new("notes-archive/today.md")
        ));
        assert!(!at_or_above(
            Path::new("notes/note"),
            Path::new("notes/note.md")
        ));
    }

    #[test]
    fn a_root_beneath_the_path_reaches_it_no_more_than_a_sibling_does() {
        assert!(!at_or_above(
            Path::new("notes/today.md"),
            Path::new("notes")
        ));
        assert!(!at_or_above(
            Path::new("notes/today.md"),
            Path::new("notes/yesterday.md")
        ));
    }

    /// An empty root is the one input the standard prefix test accepts against
    /// every path, and it is the one a caller reaches this function with by
    /// accident rather than on purpose.
    #[test]
    fn an_empty_root_is_above_nothing_though_it_is_a_prefix_of_everything() {
        assert!(Path::new("notes/today.md").starts_with(Path::new("")));
        assert!(!at_or_above(Path::new(""), Path::new("notes/today.md")));
        assert!(!at_or_above(Path::new(""), Path::new("")));
    }
}
