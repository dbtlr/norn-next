//! Whether a vault's own `.gitignore` covers the [`FALLBACK`] shadow directory.
//!
//! The fallback home sits inside the vault, so a vault kept in git commits
//! staged shadows unless it ignores them. Whether it does is a fact an operator
//! acts on, and this module is the one reading of it.
//!
//! **The reading is a narrow rule this crate owns, not git's.** Norn never
//! shells out to git and does not implement gitignore matching. It reads one
//! file — `.gitignore` at the vault root — and answers yes only where a line
//! names the fallback directory or the `.norn` directory above it in one of the
//! spellings [`covers_fallback`] lists, and no negation line names anything
//! under `.norn`. Every other arrangement answers no, which is the direction
//! that warns: a vault this rule cannot show ignores the fallback is reported
//! as not ignoring it.
//!
//! What the rule does not read, each of which can ignore the fallback where
//! this answers no:
//!
//! - a `.gitignore` in a directory above the vault root, where the vault is a
//!   subdirectory of a repository;
//! - `.git/info/exclude` and the `core.excludesFile` a git configuration names;
//! - any pattern with a wildcard other than the trailing `*` and `**` spellings
//!   listed, such as `.*` or `.n*`;
//! - a trailing space kept by a backslash escape.

use std::path::Path;

use crate::read::read_if_present_and_hash;
use crate::refusal::Refusal;
use crate::shadow::FALLBACK;

/// The name of the file this rule reads, relative to the vault root.
const GITIGNORE: &str = ".gitignore";

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
/// A vault root with no `.gitignore` covers nothing, and answers `false`. A
/// `.gitignore` that is there and cannot be read — a link, a directory, a
/// machine that refuses the read — is the refusal, and the caller decides what
/// an unanswered question means; the file is read through the same anchored,
/// link-refusing open every control file is.
pub fn fallback_ignored(vault_root: &Path) -> Result<bool, Refusal> {
    Ok(read_if_present_and_hash(vault_root, Path::new(GITIGNORE))?
        .is_some_and(|read| covers_fallback(read.bytes())))
}

/// Whether the text of a `.gitignore` covers [`FALLBACK`].
///
/// A line is read with its trailing spaces, tabs and carriage return removed;
/// a blank line and a `#` comment say nothing. The text covers the fallback
/// where some line is one of the [`COVERING`] spellings under one of the
/// [`ANCHORS`], and no line is a negation — a line opening with `!` — whose
/// pattern, its anchor removed, begins with `.norn`: git reads a negation
/// against the lines before it, which this rule does not, so a negation
/// reaching under `.norn` leaves the question unanswered and the answer is no.
/// Bytes that are not UTF-8 cover nothing.
fn covers_fallback(text: &[u8]) -> bool {
    debug_assert_eq!(
        FALLBACK, ".norn/tmp",
        "the covering spellings name FALLBACK"
    );
    let Ok(text) = std::str::from_utf8(text) else {
        return false;
    };
    let mut covered = false;
    for line in text.lines() {
        let line = line.trim_end_matches([' ', '\t', '\r']);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(negated) = line.strip_prefix('!') {
            if unanchored(negated).starts_with(".norn") {
                return false;
            }
            continue;
        }
        covered |= ANCHORS.iter().any(|anchor| {
            line.strip_prefix(anchor)
                .is_some_and(|pattern| COVERING.contains(&pattern))
        });
    }
    covered
}

/// `pattern` with the one anchor it opens with removed.
fn unanchored(pattern: &str) -> &str {
    pattern
        .strip_prefix("**/")
        .or_else(|| pattern.strip_prefix('/'))
        .unwrap_or(pattern)
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

    /// A line among others covers the fallback, whatever surrounds it, and
    /// trailing whitespace and a CRLF ending are not part of the pattern.
    #[test]
    fn a_covering_line_among_others_covers_with_its_trailing_whitespace_removed() {
        assert!(covers_fallback(
            b"# editor state\n.obsidian/\n\n.norn/ \t\r\n*.swp\n"
        ));
    }

    /// Nothing that is not a listed spelling covers the fallback: a comment,
    /// a sibling directory, a wildcard this rule does not read, a pattern
    /// naming only part of `.norn`, and a leading space.
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
        ] {
            assert!(!covers_fallback(text.as_bytes()), "{text:?} covered");
        }
    }

    /// A negation reaching under `.norn` leaves the question unanswered, so
    /// the text covers nothing whichever side of the covering line it is on.
    /// A negation elsewhere says nothing about the fallback.
    #[test]
    fn a_negation_under_norn_uncovers_and_one_elsewhere_does_not() {
        for text in [
            ".norn/\n!.norn/schema.yaml\n",
            "!/.norn/tmp/keep\n.norn/\n",
            ".norn/\n!**/.norn\n",
        ] {
            assert!(!covers_fallback(text.as_bytes()), "{text:?} covered");
        }
        assert!(covers_fallback(b".norn/\n!notes/keep.md\n"));
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
}
