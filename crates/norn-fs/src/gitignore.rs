//! Whether a vault ignores the [`FALLBACK`] shadow directory: by norn's own
//! self-ignore inside it, or by the vault's own `.gitignore`.
//!
//! The fallback home sits inside the vault, so a vault kept in git commits
//! staged shadows unless it ignores them. Whether it does is a fact an operator
//! acts on, and this module is the one reading of it.
//!
//! **Norn's own self-ignore answers first.** Making a fallback home writes
//! `.gitignore` holding `*` directly inside [`FALLBACK`]
//! ([`SELF_IGNORE`]). That pattern ignores every name beneath the fallback
//! root, and git never descends into a directory it ignores, so no
//! `.gitignore` above it or below it can re-include a shadow: where that file
//! stands at its own path holding exactly the bytes norn writes, the answer is
//! yes whatever else the vault holds, and no file of the vault's own needs
//! editing. The rule is narrow on purpose: the same bytes at any other path,
//! or other bytes at that one, are not the self-ignore — they are any
//! `.gitignore` along the home, read as the rest of this rule reads one.
//!
//! **Otherwise the reading is a narrow rule this crate owns, not git's.** Norn
//! never shells out to git and does not implement gitignore matching. It reads
//! one file — `.gitignore` at the vault root, up to [`GITIGNORE_BOUND`] bytes —
//! and answers yes only where a line names the fallback directory or the `.norn`
//! directory above it in one of the spellings [`covers_fallback`] lists, and no
//! line is a negation. Where no covering line excludes `.norn` itself, git
//! descends into `.norn` and reads the `.gitignore` of each directory it
//! enters, any of which can re-include the shadows; so the rule then answers
//! yes only where no directory along the fallback home, from `.norn` down to
//! the home itself, holds an entry named `.gitignore` of any kind. It asks
//! that of each directory with one stat and reads none of those files. Every
//! other arrangement answers no, which is the direction that warns: a vault
//! this rule cannot show ignores the fallback is reported as not ignoring it.
//! So every way the rule departs from git answers no where git ignores the
//! fallback, never yes where git does not.
//!
//! What the rule does not read, each of which can ignore the fallback where
//! this answers no:
//!
//! - a `.gitignore` in a directory above the vault root, where the vault is a
//!   subdirectory of a repository;
//! - the text of a `.gitignore` inside `.norn` other than norn's own
//!   self-ignore, which answers no by being there whatever it says;
//! - `.git/info/exclude` and the `core.excludesFile` a git configuration names;
//! - any pattern with a wildcard other than the trailing `*` and `**` spellings
//!   listed, such as `.*` or `.n*`, and a spelling git matches only because it
//!   folds case;
//! - any file holding a negation, whatever it negates;
//! - a file opening with a byte-order mark, and one that is not UTF-8;
//! - a file longer than [`GITIGNORE_BOUND`].

use std::path::{Component, Path};

use crate::read::{Bounded, read_if_present_bounded};
use crate::refusal::Refusal;
use crate::shadow::{FALLBACK, SELF_IGNORE, SELF_IGNORE_TEXT};

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
/// its contents. The first [`EXCLUDING_NORN`] of them exclude `.norn` itself,
/// so git never descends into it.
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

/// How many of the leading [`COVERING`] spellings exclude the `.norn`
/// directory itself rather than what is inside it.
const EXCLUDING_NORN: usize = 2;

/// The anchors a covering spelling may carry: none, the root, or any depth.
/// Each matches a path directly under the vault root.
const ANCHORS: &[&str] = &["", "/", "**/"];

/// Whether the vault at `vault_root` ignores the fallback home at `home`,
/// vault-relative, by the rule this module states.
///
/// Norn's own [self-ignore](SELF_IGNORE), standing at its path with exactly
/// its bytes, answers `true` for every `home` under [`FALLBACK`] before the
/// vault root's `.gitignore` is read, so a root `.gitignore` that cannot be
/// read is no refusal beside it. A self-ignore that is missing, altered or
/// not a file answers nothing itself, and the reading below answers instead.
///
/// A vault root with no `.gitignore` covers nothing, and answers `false`, and
/// so does one longer than 64 KiB, which is not read past that bound. A
/// `.gitignore` that is there and cannot be read — a link, a directory, a
/// pipe, a machine that refuses the read — is the refusal, and the caller
/// decides what an unanswered question means; the file is read through the
/// same anchored, link-refusing open every control file is, which opens a
/// pipe without waiting for its writer.
///
/// Where the covering line leaves `.norn` itself unexcluded, the answer is
/// `false` if any directory from `.norn` down to `home` holds an entry named
/// `.gitignore`, or cannot be shown not to. A `home` that is not a plain
/// vault-relative path under [`FALLBACK`] answers `false`: the directories
/// git descends into are not known.
pub fn fallback_ignored(vault_root: &Path, home: &Path) -> Result<bool, Refusal> {
    let under_fallback = under_fallback(home);
    if under_fallback && self_ignored(vault_root) {
        return Ok(true);
    }
    let coverage = match read_if_present_bounded(vault_root, Path::new(GITIGNORE), GITIGNORE_BOUND)?
    {
        Some(Bounded::Whole(bytes)) => covers_fallback(&bytes),
        Some(Bounded::Longer) | None => Coverage::None,
    };
    Ok(match coverage {
        Coverage::None => false,
        Coverage::Directory => true,
        Coverage::Contents => {
            under_fallback
                && !home
                    .ancestors()
                    .take_while(|directory| !directory.as_os_str().is_empty())
                    .any(|directory| gitignore_present(&vault_root.join(directory)))
        }
    })
}

/// Whether `home` is a plain vault-relative path under [`FALLBACK`]: the
/// directories git descends into on the way to it are then known.
fn under_fallback(home: &Path) -> bool {
    home.starts_with(FALLBACK)
        && home
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// Whether norn's own [self-ignore](SELF_IGNORE) stands in the fallback root
/// beneath `vault_root`, holding exactly the bytes norn writes there.
///
/// Read through the same anchored, link-refusing open the root `.gitignore`
/// is, bounded at those bytes. Anything else — nothing at the name, other
/// bytes, a link, a directory, a read the machine refuses — is not the
/// self-ignore, and the rest of the rule answers instead.
fn self_ignored(vault_root: &Path) -> bool {
    let at = Path::new(FALLBACK).join(SELF_IGNORE);
    matches!(
        read_if_present_bounded(vault_root, &at, SELF_IGNORE_TEXT.len()),
        Ok(Some(Bounded::Whole(bytes))) if bytes == SELF_IGNORE_TEXT
    )
}

/// Whether `directory` holds an entry named `.gitignore`, of any kind, or
/// cannot be shown not to: only a stat that finds nothing there answers no.
#[allow(clippy::disallowed_methods)] // The vault filesystem seam: this crate owns vault stat.
fn gitignore_present(directory: &Path) -> bool {
    !matches!(
        std::fs::symlink_metadata(directory.join(GITIGNORE)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    )
}

/// What the lines of a `.gitignore` cover of [`FALLBACK`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Coverage {
    /// No line covers it, or the text leaves the question unanswered.
    None,
    /// A line excludes the `.norn` directory itself.
    Directory,
    /// A line covers the fallback from inside `.norn`, which git descends
    /// into.
    Contents,
}

/// What the text of a `.gitignore` covers of [`FALLBACK`].
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
/// are not UTF-8 cover nothing. A line excluding `.norn` itself outranks one
/// covering what is inside it.
fn covers_fallback(text: &[u8]) -> Coverage {
    debug_assert_eq!(
        FALLBACK, ".norn/tmp",
        "the covering spellings name FALLBACK"
    );
    let Ok(text) = std::str::from_utf8(text) else {
        return Coverage::None;
    };
    let mut coverage = Coverage::None;
    for line in text.split('\n') {
        let line = line
            .strip_suffix('\r')
            .unwrap_or(line)
            .trim_end_matches(' ');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('!') {
            return Coverage::None;
        }
        let spelling = ANCHORS.iter().find_map(|anchor| {
            let pattern = line.strip_prefix(anchor)?;
            COVERING.iter().position(|spelling| *spelling == pattern)
        });
        coverage = match (coverage, spelling) {
            (Coverage::Directory, _) => Coverage::Directory,
            (_, Some(index)) if index < EXCLUDING_NORN => Coverage::Directory,
            (_, Some(_)) => Coverage::Contents,
            (coverage, None) => coverage,
        };
    }
    coverage
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
                assert_ne!(
                    covers_fallback(line.as_bytes()),
                    Coverage::None,
                    "{line:?} did not cover"
                );
            }
        }
    }

    /// **Only `.norn` and `.norn/` exclude the `.norn` directory itself**,
    /// under every anchor, and such a line outranks a line covering what is
    /// inside it wherever the two stand.
    #[test]
    fn only_the_norn_directory_spellings_exclude_it_whatever_else_stands() {
        for anchor in ANCHORS {
            for (index, spelling) in COVERING.iter().enumerate() {
                let expected = if index < EXCLUDING_NORN {
                    Coverage::Directory
                } else {
                    Coverage::Contents
                };
                let line = format!("{anchor}{spelling}\n");
                assert_eq!(covers_fallback(line.as_bytes()), expected, "{line:?}");
            }
        }
        for text in [".norn/tmp/*\n.norn/\n", ".norn\n.norn/tmp/*\n"] {
            assert_eq!(
                covers_fallback(text.as_bytes()),
                Coverage::Directory,
                "{text:?}"
            );
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
            assert_ne!(
                covers_fallback(text.as_bytes()),
                Coverage::None,
                "{text:?} did not cover"
            );
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
            assert_eq!(
                covers_fallback(text.as_bytes()),
                Coverage::None,
                "{text:?} covered"
            );
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
            assert_eq!(
                covers_fallback(text.as_bytes()),
                Coverage::None,
                "{text:?} covered"
            );
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
            assert_eq!(
                covers_fallback(text.as_bytes()),
                Coverage::None,
                "{text:?} covered"
            );
        }
    }

    /// Bytes that are not text cover nothing.
    #[test]
    fn bytes_that_are_not_utf8_cover_nothing() {
        assert_eq!(covers_fallback(b".norn/\n\xff\n"), Coverage::None);
    }

    /// The vault root's `.gitignore` is what is read: a covering one answers
    /// yes, and a vault with none answers no.
    #[test]
    fn the_vault_roots_gitignore_is_read() {
        let scratch = Scratch::new("gitignore-read");
        let root = scratch.at("");
        assert_eq!(fallback_ignored(&root, Path::new(HOME)), Ok(false));
        scratch.place(GITIGNORE, b".norn/\n");
        assert_eq!(fallback_ignored(&root, Path::new(HOME)), Ok(true));
        scratch.place(GITIGNORE, b"notes/\n");
        assert_eq!(fallback_ignored(&root, Path::new(HOME)), Ok(false));
    }

    /// The fallback home a nested case stages under, vault-relative: the
    /// channel, vault and key directories below [`FALLBACK`].
    const HOME: &str = ".norn/tmp/norn-dev/notes/0123456789abcdef";

    /// A scratch vault holding the fallback home, its root `.gitignore`
    /// reading `root`, and `nested` placed at `at`.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging the tree a case reads.
    fn nested_case(label: &str, root: &str, at: &str, nested: &str) -> Scratch {
        let scratch = Scratch::new(label);
        std::fs::create_dir_all(scratch.at(HOME)).unwrap();
        scratch.place(GITIGNORE, format!("{root}\n").as_bytes());
        scratch.place(at, format!("{nested}\n").as_bytes());
        scratch
    }

    /// **A `.gitignore` inside the `.norn` directory answers no where the
    /// covering line leaves `.norn` itself unexcluded**: git descends into
    /// every directory along the home the line does not exclude and reads
    /// each one's `.gitignore`, which can re-include the shadows. Each case
    /// is one git does not ignore.
    #[test]
    fn a_nested_gitignore_answers_no_where_the_norn_directory_is_descended_into() {
        for (root, at, nested) in [
            (".norn/*", ".norn/.gitignore", "!tmp"),
            (".norn/tmp/", ".norn/.gitignore", "!tmp/"),
            (".norn/tmp", ".norn/.gitignore", "!tmp"),
            (".norn/tmp/*", ".norn/tmp/.gitignore", "!*"),
            (".norn/tmp/**", ".norn/tmp/.gitignore", "!*"),
            (".norn/**", ".norn/.gitignore", "!*"),
            ("**/.norn/*", ".norn/.gitignore", "!tmp"),
        ] {
            let scratch = nested_case("gitignore-nested", root, at, nested);
            assert_eq!(
                fallback_ignored(&scratch.at(""), Path::new(HOME)),
                Ok(false),
                "{root:?} with {at} = {nested:?} answered ignored"
            );
        }
    }

    /// **A `.gitignore` anywhere along the home answers no** under a line
    /// that does not exclude `.norn`, whatever its kind: the rule asks only
    /// whether an entry is there, down to the key's own directory.
    #[cfg(unix)]
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: planting what a case reads.
    fn a_gitignore_of_any_kind_at_any_level_of_the_home_answers_no() {
        for level in [
            ".norn",
            ".norn/tmp",
            ".norn/tmp/norn-dev",
            ".norn/tmp/norn-dev/notes",
            HOME,
        ] {
            let at = format!("{level}/{GITIGNORE}");
            let scratch = nested_case("gitignore-level", ".norn/tmp/*", &at, "");
            let root = scratch.at("");
            let home = Path::new(HOME);
            assert_eq!(fallback_ignored(&root, home), Ok(false), "a file at {at}");
            std::fs::remove_file(scratch.at(&at)).unwrap();
            assert_eq!(fallback_ignored(&root, home), Ok(true), "nothing at {at}");
            std::fs::create_dir(scratch.at(&at)).unwrap();
            assert_eq!(
                fallback_ignored(&root, home),
                Ok(false),
                "a directory at {at}"
            );
            std::fs::remove_dir(scratch.at(&at)).unwrap();
            std::os::unix::fs::symlink("elsewhere", scratch.at(&at)).unwrap();
            assert_eq!(fallback_ignored(&root, home), Ok(false), "a link at {at}");
        }
    }

    /// **A line excluding `.norn` itself answers yes whatever lies inside
    /// it**: git does not descend into an excluded directory, so no
    /// `.gitignore` under it is read.
    #[test]
    fn a_line_excluding_the_norn_directory_answers_yes_over_any_nested_gitignore() {
        for root in [".norn", ".norn/", "/.norn", "**/.norn/"] {
            for at in [".norn/.gitignore", ".norn/tmp/.gitignore"] {
                let scratch = nested_case("gitignore-shut", root, at, "!*");
                assert_eq!(
                    fallback_ignored(&scratch.at(""), Path::new(HOME)),
                    Ok(true),
                    "{root:?} with {at} answered not ignored"
                );
            }
        }
    }

    /// Norn's own self-ignore, vault-relative.
    const SELF_IGNORE_AT: &str = ".norn/tmp/.gitignore";

    /// A scratch vault holding the fallback home and norn's self-ignore
    /// beside it, its root `.gitignore` reading `root` where there is one.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging the tree a case reads.
    fn self_ignored_case(label: &str, root: Option<&str>) -> Scratch {
        let scratch = Scratch::new(label);
        std::fs::create_dir_all(scratch.at(HOME)).unwrap();
        scratch.place(SELF_IGNORE_AT, b"*\n");
        if let Some(root) = root {
            scratch.place(GITIGNORE, format!("{root}\n").as_bytes());
        }
        scratch
    }

    /// **Norn's own self-ignore covers the fallback whatever the vault's
    /// `.gitignore` says**: `*` at `.norn/tmp` ignores every name beneath it,
    /// and git never descends into a directory it ignores, so no `.gitignore`
    /// above or below it re-includes a shadow. A vault with no `.gitignore`,
    /// one covering nothing, one covering only what is inside `.norn`, and
    /// one that cannot be read all answer yes.
    #[cfg(unix)]
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: planting the link a case reads.
    fn norns_own_self_ignore_covers_the_fallback_whatever_the_root_says() {
        for root in [None, Some("notes/"), Some(".norn/tmp/*"), Some("!*")] {
            let scratch = self_ignored_case("gitignore-self", root);
            assert_eq!(
                fallback_ignored(&scratch.at(""), Path::new(HOME)),
                Ok(true),
                "the self-ignore under a root reading {root:?} answered not ignored"
            );
        }
        let scratch = self_ignored_case("gitignore-self-link", None);
        scratch.place("elsewhere", b"notes/\n");
        std::os::unix::fs::symlink("elsewhere", scratch.at(GITIGNORE)).unwrap();
        assert_eq!(fallback_ignored(&scratch.at(""), Path::new(HOME)), Ok(true));
    }

    /// **Only norn's exact bytes count as its self-ignore**: a file at its
    /// name holding anything else is an alteration, which is read as any
    /// `.gitignore` standing along the home is — no with no root line
    /// excluding `.norn` itself — and so is a missing one.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: altering what a case reads.
    fn an_altered_or_missing_self_ignore_does_not_cover_the_fallback() {
        for altered in [
            &b""[..],
            b"*",
            b"* \n",
            b"*\r\n",
            b"**\n",
            b"*\n*\n",
            b"*\n!norn-dev\n",
            b"!*\n",
        ] {
            let scratch = self_ignored_case("gitignore-self-altered", None);
            scratch.place(SELF_IGNORE_AT, altered);
            assert_eq!(
                fallback_ignored(&scratch.at(""), Path::new(HOME)),
                Ok(false),
                "{:?} counted as the self-ignore",
                String::from_utf8_lossy(altered)
            );
        }
        let scratch = self_ignored_case("gitignore-self-missing", Some(".norn/tmp/*"));
        std::fs::remove_file(scratch.at(SELF_IGNORE_AT)).unwrap();
        assert_eq!(fallback_ignored(&scratch.at(""), Path::new(HOME)), Ok(true));
        std::fs::remove_file(scratch.at(GITIGNORE)).unwrap();
        assert_eq!(
            fallback_ignored(&scratch.at(""), Path::new(HOME)),
            Ok(false)
        );
    }

    /// **Only the file at the self-ignore's own path counts**: the same bytes
    /// in `.norn` above it or in a directory below it are any `.gitignore`
    /// along the home, and a directory at the self-ignore's name is not one.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging the tree a case reads.
    fn the_self_ignore_counts_only_at_its_own_path() {
        for at in [".norn/.gitignore", ".norn/tmp/norn-dev/.gitignore"] {
            let scratch = Scratch::new("gitignore-self-elsewhere");
            std::fs::create_dir_all(scratch.at(HOME)).unwrap();
            scratch.place(at, b"*\n");
            assert_eq!(
                fallback_ignored(&scratch.at(""), Path::new(HOME)),
                Ok(false),
                "the self-ignore's bytes at {at} counted"
            );
        }
        let scratch = Scratch::new("gitignore-self-directory");
        std::fs::create_dir_all(scratch.at(HOME)).unwrap();
        std::fs::create_dir(scratch.at(SELF_IGNORE_AT)).unwrap();
        assert_eq!(
            fallback_ignored(&scratch.at(""), Path::new(HOME)),
            Ok(false)
        );
    }

    /// A home that is not under [`FALLBACK`] answers no beside the
    /// self-ignore too: it covers only what is beneath the fallback root.
    #[test]
    fn the_self_ignore_covers_no_home_outside_the_fallback() {
        let scratch = self_ignored_case("gitignore-self-home", None);
        let root = scratch.at("");
        for home in [
            "elsewhere/key",
            ".norn/key",
            "/abs/.norn/tmp/key",
            ".norn/tmp/../key",
        ] {
            assert_eq!(
                fallback_ignored(&root, Path::new(home)),
                Ok(false),
                "{home}"
            );
        }
    }

    /// A home that is not under [`FALLBACK`], vault-relative, answers no:
    /// the directories git would descend into are not known.
    #[test]
    fn a_home_outside_the_fallback_answers_no() {
        let scratch = Scratch::new("gitignore-home");
        scratch.place(GITIGNORE, b".norn/tmp/*\n");
        let root = scratch.at("");
        assert_eq!(fallback_ignored(&root, Path::new(HOME)), Ok(true));
        for home in [
            "elsewhere/key",
            ".norn/key",
            "/abs/.norn/tmp/key",
            ".norn/tmp/../key",
        ] {
            assert_eq!(
                fallback_ignored(&root, Path::new(home)),
                Ok(false),
                "{home}"
            );
        }
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
        assert_eq!(fallback_ignored(&root, Path::new(HOME)), Ok(true));
        text.push(b'\n');
        scratch.place(GITIGNORE, &text);
        assert_eq!(fallback_ignored(&root, Path::new(HOME)), Ok(false));
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
        assert!(fallback_ignored(&root, Path::new(HOME)).is_err());
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
        assert!(fallback_ignored(&root, Path::new(HOME)).is_err());
        std::fs::remove_dir(scratch.at(GITIGNORE)).unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(scratch.at(GITIGNORE))
            .status()
            .expect("run mkfifo");
        assert!(made.success(), "mkfifo failed");
        assert!(fallback_ignored(&root, Path::new(HOME)).is_err());
    }
}
