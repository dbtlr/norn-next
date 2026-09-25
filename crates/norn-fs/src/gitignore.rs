//! Whether a vault's own `.gitignore` covers the [`FALLBACK`] shadow directory.
//!
//! The fallback home sits inside the vault, so a vault kept in git commits
//! staged shadows unless it ignores them. Whether it does is a fact an operator
//! acts on, and this module is the one reading of it.
//!
//! **The reading is a narrow rule this crate owns, not git's.** Norn never
//! shells out to git and does not implement gitignore matching. It reads one
//! file — `.gitignore` at the vault root, up to [`GITIGNORE_BOUND`] bytes — and
//! answers yes only where a line names the fallback directory or the `.norn`
//! directory above it in one of the spellings [`covers_fallback`] lists, and no
//! line is a negation. Every other arrangement answers no, which is the
//! direction that warns: a vault this rule cannot show ignores the fallback is
//! reported as not ignoring it. So every way the rule departs from git answers
//! no where git ignores the fallback, never yes where git does not.
//!
//! What the rule does not read, each of which can ignore the fallback where
//! this answers no:
//!
//! - a `.gitignore` in a directory above the vault root, where the vault is a
//!   subdirectory of a repository;
//! - `.git/info/exclude` and the `core.excludesFile` a git configuration names;
//! - any pattern with a wildcard other than the trailing `*` and `**` spellings
//!   listed, such as `.*` or `.n*`, and a spelling git matches only because it
//!   folds case;
//! - any file holding a negation, whatever it negates;
//! - a file opening with a byte-order mark, and one that is not UTF-8;
//! - a file longer than [`GITIGNORE_BOUND`].

use std::path::Path;

use crate::read::{Bounded, read_if_present_bounded};
use crate::refusal::Refusal;
use crate::shadow::FALLBACK;

/// The name of the file this rule reads, relative to the vault root.
const GITIGNORE: &str = ".gitignore";

/// The most bytes of a `.gitignore` this rule reads.
///
/// An authored bound rather than a measured one. A `.gitignore` a person keeps
/// is a few hundred bytes, and this is two orders of magnitude past that; a
/// file past it answers no, unread, so what a `vault status` pays for the
/// reading is bounded whatever the vault holds at the name.
const GITIGNORE_BOUND: usize = 64 * 1024;

/// The spellings of a line, before its anchor, that cover the fallback
/// directory: the `.norn` directory, its contents, the fallback directory and
/// its contents.
const COVERING: &[&str] = &[
    ".norn",
    ".norn/",
    ".norn/*",
    ".norn/**",
    ".norn/tmp",
    ".norn/tmp/",
    ".norn/tmp/*",
    ".norn/tmp/**",
];

/// The anchors a covering spelling may carry: none, the root, or any depth.
/// Each matches a path directly under the vault root.
const ANCHORS: &[&str] = &["", "/", "**/"];

/// Whether the `.gitignore` at `vault_root` covers [`FALLBACK`], by the rule
/// this module states.
///
/// A vault root with no `.gitignore` covers nothing, and answers `false`, and
/// so does one longer than [`GITIGNORE_BOUND`]. A `.gitignore` that is there
/// and cannot be read — a link, a directory, a pipe, a machine that refuses the
/// read — is the refusal, and the caller decides what an unanswered question
/// means; the file is read through the same anchored, link-refusing open every
/// control file is, which opens a pipe without waiting for its writer.
pub fn fallback_ignored(vault_root: &Path) -> Result<bool, Refusal> {
    Ok(
        match read_if_present_bounded(vault_root, Path::new(GITIGNORE), GITIGNORE_BOUND)? {
            Some(Bounded::Whole(bytes)) => covers_fallback(&bytes),
            Some(Bounded::Longer) | None => false,
        },
    )
}

/// Whether the text of a `.gitignore` covers [`FALLBACK`].
///
/// A line is read as git reads it: one carriage return ending it is removed,
/// then its trailing spaces. A tab is kept, and so is a space a backslash
/// escapes — both are part of the pattern, which then names no spelling
/// listed here. A blank line and a `#` comment say nothing. The text covers
/// the fallback where some line is one of the [`COVERING`] spellings under one
/// of the [`ANCHORS`], and no line is a negation — a line opening with `!`:
/// git reads a negation against the lines before it and can re-include the
/// fallback through a wildcard or an anchor this rule does not read, so any
/// negation leaves the question unanswered and the answer is no. Bytes that
/// are not UTF-8 cover nothing.
fn covers_fallback(text: &[u8]) -> bool {
    debug_assert_eq!(
        FALLBACK, ".norn/tmp",
        "the covering spellings name FALLBACK"
    );
    let Ok(text) = std::str::from_utf8(text) else {
        return false;
    };
    let mut covered = false;
    for line in text.split('\n') {
        let line = line
            .strip_suffix('\r')
            .unwrap_or(line)
            .trim_end_matches(' ');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('!') {
            return false;
        }
        covered |= ANCHORS.iter().any(|anchor| {
            line.strip_prefix(anchor)
                .is_some_and(|pattern| COVERING.contains(&pattern))
        });
    }
    covered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;

    /// Every listed spelling of the `.norn` directory and of the fallback
    /// directory covers the fallback, under every anchor.
    #[test]
    fn every_listed_spelling_under_every_anchor_covers_the_fallback() {
        for anchor in ANCHORS {
            for spelling in COVERING {
                let line = format!("{anchor}{spelling}\n");
                assert!(covers_fallback(line.as_bytes()), "{line:?} did not cover");
            }
        }
    }

    /// A covering line among others covers the fallback with its trailing
    /// spaces removed, with one carriage return before its line feed
    /// removed, and as the last line with no line feed at all.
    #[test]
    fn a_covering_line_covers_with_its_trailing_spaces_and_one_carriage_return_removed() {
        for text in [
            "# editor state\n.obsidian/\n\n.norn/  \n*.swp\n",
            ".obsidian/\r\n.norn/\r\n",
            ".norn/\r",
            ".norn/",
        ] {
            assert!(covers_fallback(text.as_bytes()), "{text:?} did not cover");
        }
    }

    /// **Only trailing spaces and one carriage return are removed**, as git
    /// removes them: a trailing tab, a second carriage return and a trailing
    /// space kept by a backslash are part of the pattern, which then names
    /// another directory.
    #[test]
    fn a_trailing_tab_a_second_carriage_return_and_an_escaped_space_cover_nothing() {
        for text in [
            ".norn/\t\n",
            ".norn/ \t\n",
            ".norn/\r\r\n",
            ".norn/\r\r",
            ".norn/\\ \n",
            ".norn\\ \n",
        ] {
            assert!(!covers_fallback(text.as_bytes()), "{text:?} covered");
        }
    }

    /// Nothing that is not a listed spelling covers the fallback: a comment,
    /// a sibling directory, a wildcard this rule does not read, a pattern
    /// naming only part of `.norn`, a leading space, a name without its dot,
    /// and an anchor one level deep, which git does not match at the root.
    #[test]
    fn a_pattern_outside_the_listed_spellings_covers_nothing() {
        for text in [
            "",
            "# .norn/\n",
            ".obsidian/\n",
            ".*\n",
            ".n*\n",
            ".norn/schema.yaml\n",
            ".norn/tmp/notes\n",
            " .norn/\n",
            "notes/.norn/\n",
            "norn\n",
            "norn/\n",
            "/norn\n",
            "**/norn/\n",
            "*/.norn\n",
            "*/.norn/\n",
            "*/.norn/tmp\n",
        ] {
            assert!(!covers_fallback(text.as_bytes()), "{text:?} covered");
        }
    }

    /// **Any negation leaves the question unanswered**, so the text covers
    /// nothing, wherever the negation stands and whatever it names: git
    /// reads a negation against the lines before it and re-includes through
    /// wildcards and anchors this rule does not read.
    #[test]
    fn any_negation_leaves_the_fallback_uncovered() {
        for text in [
            ".norn/\n!.norn/schema.yaml\n",
            "!/.norn/tmp/keep\n.norn/\n",
            ".norn/\n!**/.norn\n",
            ".norn/\n!*\n",
            ".norn/\n!*/\n",
            ".norn/\n!.*\n",
            ".norn/tmp/\n!tmp\n",
            ".norn/tmp/\n!\\.norn/tmp\n",
            ".norn/tmp/\n!/**/.norn/tmp\n",
            ".norn/*\n!tmp/\n",
            ".norn/\n!notes/keep.md\n",
        ] {
            assert!(!covers_fallback(text.as_bytes()), "{text:?} covered");
        }
    }

    /// Bytes that are not text cover nothing.
    #[test]
    fn bytes_that_are_not_utf8_cover_nothing() {
        assert!(!covers_fallback(b".norn/\n\xff\n"));
    }

    /// The vault root's `.gitignore` is what is read: a covering one answers
    /// yes, and a vault with none answers no.
    #[test]
    fn the_vault_roots_gitignore_is_read() {
        let scratch = Scratch::new("gitignore-read");
        let root = scratch.at("");
        assert_eq!(fallback_ignored(&root), Ok(false));
        scratch.place(GITIGNORE, b".norn/\n");
        assert_eq!(fallback_ignored(&root), Ok(true));
        scratch.place(GITIGNORE, b"notes/\n");
        assert_eq!(fallback_ignored(&root), Ok(false));
    }

    /// **A `.gitignore` longer than the bound is not read past it, and
    /// answers no**, whatever its first bytes say; one at the bound is read
    /// whole.
    #[test]
    fn a_gitignore_past_the_bound_answers_no() {
        let scratch = Scratch::new("gitignore-bound");
        let root = scratch.at("");
        let mut text = b".norn/\n".to_vec();
        text.resize(GITIGNORE_BOUND, b'\n');
        scratch.place(GITIGNORE, &text);
        assert_eq!(fallback_ignored(&root), Ok(true));
        text.push(b'\n');
        scratch.place(GITIGNORE, &text);
        assert_eq!(fallback_ignored(&root), Ok(false));
    }

    /// A `.gitignore` that is a link is a refusal rather than an answer: the
    /// read is anchored and follows no link, as every control file's is.
    #[cfg(unix)]
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: planting the link a case reads.
    fn a_linked_gitignore_is_refused() {
        let scratch = Scratch::new("gitignore-link");
        let root = scratch.at("");
        scratch.place("elsewhere", b".norn/\n");
        std::os::unix::fs::symlink("elsewhere", scratch.at(GITIGNORE)).unwrap();
        assert!(fallback_ignored(&root).is_err());
    }

    /// A `.gitignore` that is a directory or a named pipe is a refusal, and
    /// the pipe is refused without waiting for a writer.
    #[cfg(unix)]
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: planting what a case reads.
    fn a_gitignore_that_is_a_directory_or_a_pipe_is_refused_without_waiting() {
        let scratch = Scratch::new("gitignore-kinds");
        let root = scratch.at("");
        std::fs::create_dir(scratch.at(GITIGNORE)).unwrap();
        assert!(fallback_ignored(&root).is_err());
        std::fs::remove_dir(scratch.at(GITIGNORE)).unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(scratch.at(GITIGNORE))
            .status()
            .expect("run mkfifo");
        assert!(made.success(), "mkfifo failed");
        assert!(fallback_ignored(&root).is_err());
    }
}
