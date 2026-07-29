//! The lane assignment is the `#[ignore]` reason, so the reason is checked.
//!
//! Two CI lanes adopt this crate's ignored cases by asking for them wholesale:
//! the per-PR memory job and the nightly soak lane each run a suite's ignored
//! cases and assert that a non-zero number of them passed. That is what makes
//! a name filter unnecessary, and it is also what makes a stray `#[ignore]`
//! dangerous — an `#[ignore = "flaky"]` added for a local reason would be
//! silently adopted by whichever lane runs that suite, and would run nightly
//! under a bar nobody meant it to face.
//!
//! So the reason is not free text, and it is bound to the *file* it sits in,
//! not just to a set of sanctioned spellings: [`LANE_BY_FILE_STEM`] maps each
//! test file's stem to the one lane prefix its `#[ignore]` reasons may open
//! with, and this case reads the sources to say so. A stem the map does not
//! name is an ERROR — unknown, never lane-less — so adding a lane's test file
//! forces a map edit in the same diff as the workflow step that runs it, and a
//! prefix sanctioned for a different file is rejected exactly as loudly as one
//! that is not sanctioned at all.
//!
//! A second sweep catches the shapes the primary matcher cannot parse:
//! `#[cfg_attr(unix, ignore = "...")]` never starts a trimmed line with
//! `#[ignore`, and `#[test] #[ignore = "..."]` on one line puts the ignore
//! attribute second. Both are real `#[ignore]`s a test binary honors, so
//! failing to recognize either would let an unnamed-lane case run silently.
//! An unrecognized shape fails the check rather than being skipped — this
//! crate does not parse every attribute grammar, so a line it cannot classify
//! as a plain, checkable `#[ignore = "..."]` is treated as unsafe rather than
//! passed through.

use std::path::{Path, PathBuf};

/// Which lane prefix a file's `#[ignore]` reasons must open with, keyed by
/// the file's stem — its name under `tests/` without the `.rs` extension.
///
/// - `soak-lane case:` — the nightly soak workflow's `--ignored` steps.
/// - `memory-lane case:` — the per-PR `memory invariant` job's `--ignored`
///   step.
///
/// Adding a lane's test file means adding its stem here, in the same diff as
/// the workflow step that runs it: a stem this table does not name is an
/// error, not a case with no lane to check against.
const LANE_BY_FILE_STEM: &[(&str, &str)] = &[
    ("memory", "memory-lane case:"),
    ("memory_soak", "soak-lane case:"),
    ("determinism", "soak-lane case:"),
    ("calibration", "soak-lane case:"),
];

/// Every `.rs` file under this crate's `tests/` directory.
///
/// The crate's own manifest directory is the root, so the walk is over this
/// package's sources and never over a path a caller supplied.
#[allow(clippy::disallowed_methods)] // Reads this crate's own test sources, from its own manifest directory.
fn test_sources() -> Vec<PathBuf> {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        let entries = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("reading {} for test sources: {e}", dir.display()));
        for entry in entries {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.extension().is_some_and(|e| e == "rs") {
                found.push(path);
            }
        }
    }

    let mut found = Vec::new();
    walk(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests"),
        &mut found,
    );
    found.sort();
    found
}

/// Every `#[ignore...]` attribute in `source`, as `(line number, attribute)`.
fn ignore_attributes(source: &str) -> Vec<(usize, &str)> {
    source
        .lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line.trim()))
        .filter(|(_, line)| line.starts_with("#[ignore"))
        .collect()
}

/// The reason text of `#[ignore = "..."]`, or `None` for any other spelling.
fn reason(attribute: &str) -> Option<&str> {
    let (_, rest) = attribute.split_once('"')?;
    let (reason, _) = rest.split_once('"')?;
    Some(reason)
}

/// Whether `reason` opens with the lane prefix `stem`'s file is bound to.
///
/// `Err` describes what is wrong: a stem [`LANE_BY_FILE_STEM`] does not name,
/// a sanctioned prefix used in a file it is not bound to, or a reason with no
/// lane prefix at all.
fn check_ignore_reason(stem: &str, reason: &str) -> Result<(), String> {
    let Some(&(_, required)) = LANE_BY_FILE_STEM.iter().find(|(s, _)| *s == stem) else {
        return Err(format!(
            "no lane is named for file stem `{stem}` in LANE_BY_FILE_STEM. Adding a lane's test \
             file means adding it here, in the same diff as the workflow step that runs it — an \
             unnamed stem is an error, not a case with no lane."
        ));
    };
    if reason.starts_with(required) {
        return Ok(());
    }
    if LANE_BY_FILE_STEM
        .iter()
        .any(|(_, prefix)| reason.starts_with(prefix))
    {
        return Err(format!(
            "the ignore reason {reason:?} opens with a sanctioned lane prefix, but not the one \
             `{stem}.rs` is bound to (`{required}`) in LANE_BY_FILE_STEM. A sanctioned prefix in \
             the wrong file is adopted by the wrong lane."
        ));
    }
    Err(format!(
        "the ignore reason {reason:?} opens with no lane prefix. `{stem}.rs` is bound to \
         `{required}` in LANE_BY_FILE_STEM, because a CI step runs a suite's ignored cases \
         wholesale and would adopt this one."
    ))
}

/// Attribute-opening lines the primary matcher in [`ignore_attributes`] did
/// not claim, but which still mention `ignore` in a shape this checker does
/// not parse — as `(line number, line)`.
///
/// Scoped to lines whose trimmed text opens an attribute (`#[...]`), so a
/// doc comment or a string literal quoting `#[ignore]` for illustration —
/// this file's own unit tests do exactly that — is never mistaken for a real
/// attribute: neither ever opens a trimmed line with `#[`.
fn unrecognized_ignore_attribute_lines(source: &str) -> Vec<(usize, &str)> {
    source
        .lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line.trim()))
        .filter(|(_, line)| line.starts_with("#[") && !line.starts_with("#[ignore"))
        .filter(|(_, line)| mentions_ignore(line))
        .collect()
}

/// Whether an attribute-opening line mentions `ignore` as a reason key
/// (`ignore = "`) or as a bare token set off by non-identifier characters, so
/// `ignored` and `ignorance` never match.
fn mentions_ignore(line: &str) -> bool {
    if line.contains("ignore = \"") {
        return true;
    }
    line.match_indices("ignore").any(|(start, matched)| {
        let before_is_ident = line[..start].chars().next_back().is_some_and(is_ident_char);
        let after_is_ident = line[start + matched.len()..]
            .chars()
            .next()
            .is_some_and(is_ident_char);
        !before_is_ident && !after_is_ident
    })
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[test]
#[allow(clippy::disallowed_methods)] // Reads this crate's own test sources, from its own manifest directory.
fn every_ignored_case_names_the_lane_that_adopts_it() {
    let mut checked = 0usize;
    for path in test_sources() {
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_else(|| panic!("{}: not a UTF-8 file stem", path.display()));

        for (line, attribute) in ignore_attributes(&source) {
            let where_it_is = format!("{}:{line}", path.display());
            let reason = reason(attribute).unwrap_or_else(|| {
                panic!(
                    "{where_it_is}: `{attribute}` carries no reason. An ignored case states the \
                     lane that adopts it, because a CI step runs a suite's ignored cases \
                     wholesale."
                )
            });
            if let Err(problem) = check_ignore_reason(stem, reason) {
                panic!("{where_it_is}: {problem}");
            }
            checked += 1;
        }

        let unrecognized = unrecognized_ignore_attribute_lines(&source);
        if !unrecognized.is_empty() {
            let lines = unrecognized
                .into_iter()
                .map(|(line, text)| format!("{}:{line}: `{text}`", path.display()))
                .collect::<Vec<_>>()
                .join("\n");
            panic!(
                "these lines mention `ignore` in a shape this checker does not parse:\n{lines}\n\
                 Only a plain `#[ignore = \"<lane prefix> case: ...\"]`, alone and first on its \
                 own line, is recognized — rewrite each to that shape so the lane it adopts is \
                 checked instead of silently allowed through."
            );
        }
    }
    assert!(
        checked > 0,
        "this crate's test sources hold no `#[ignore]` at all, so the guard checked nothing \
         and the lanes it protects run nothing"
    );
}

#[cfg(test)]
mod tests {
    use super::{
        check_ignore_reason, ignore_attributes, reason, unrecognized_ignore_attribute_lines,
    };

    #[test]
    fn an_attribute_is_found_wherever_it_is_indented() {
        let source = "#[test]\n    #[ignore = \"soak-lane case: x\"]\nfn f() {}\n";
        assert_eq!(
            ignore_attributes(source),
            vec![(2, "#[ignore = \"soak-lane case: x\"]")]
        );
    }

    #[test]
    fn a_bare_ignore_yields_no_reason() {
        assert_eq!(reason("#[ignore]"), None);
        assert_eq!(reason("#[ignore = \"flaky\"]"), Some("flaky"));
    }

    #[test]
    fn a_reason_matching_its_files_lane_is_accepted() {
        assert!(check_ignore_reason("memory", "memory-lane case: x").is_ok());
        for stem in ["memory_soak", "determinism", "calibration"] {
            assert!(
                check_ignore_reason(stem, "soak-lane case: x").is_ok(),
                "{stem} should accept a soak-lane reason"
            );
        }
    }

    #[test]
    fn a_sanctioned_prefix_in_the_wrong_file_is_rejected() {
        let problem = check_ignore_reason("memory", "soak-lane case: x")
            .expect_err("a soak-lane reason does not belong to memory.rs");
        assert!(
            problem.contains("memory-lane case:"),
            "the error should name the prefix memory.rs is bound to: {problem}"
        );
    }

    #[test]
    fn an_unmapped_file_stem_is_rejected() {
        let problem = check_ignore_reason("mystery", "soak-lane case: x")
            .expect_err("a stem with no map entry has nothing to check against");
        assert!(problem.contains("mystery"), "{problem}");
    }

    #[test]
    fn a_reason_with_no_lane_prefix_at_all_is_rejected() {
        let problem = check_ignore_reason("memory", "flaky")
            .expect_err("a reason with no lane prefix cannot be accepted");
        assert!(problem.contains("memory-lane case:"), "{problem}");
    }

    #[test]
    fn a_cfg_attr_wrapped_ignore_is_flagged_as_unrecognized() {
        let source = "#[test]\n#[cfg_attr(unix, ignore = \"soak-lane case: x\")]\nfn f() {}\n";
        assert_eq!(
            unrecognized_ignore_attribute_lines(source),
            vec![(2, "#[cfg_attr(unix, ignore = \"soak-lane case: x\")]")]
        );
    }

    #[test]
    fn a_shared_line_ignore_is_flagged_as_unrecognized() {
        let source = "#[test] #[ignore = \"soak-lane case: x\"]\nfn f() {}\n";
        assert_eq!(
            unrecognized_ignore_attribute_lines(source),
            vec![(1, "#[test] #[ignore = \"soak-lane case: x\"]")]
        );
    }

    #[test]
    fn a_doc_comment_quoting_ignore_is_never_flagged() {
        let source = "//! The `#[ignore]` reason names the lane, ignore or not.\n";
        assert!(unrecognized_ignore_attribute_lines(source).is_empty());
    }
}
