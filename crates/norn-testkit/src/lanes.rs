//! The lane assignment is the `#[ignore]` reason, so the reason is checked.
//!
//! CI lanes adopt a crate's ignored cases by asking for them wholesale: each
//! step runs a suite's ignored cases and asserts that a non-zero number of them
//! passed. That is what makes a name filter unnecessary, and it is also what
//! makes a stray `#[ignore]` dangerous — an `#[ignore = "flaky"]` added for a
//! local reason would be silently adopted by whichever lane runs that suite,
//! and would run under a bar nobody meant it to face.
//!
//! So the reason is not free text, and it is bound to the *file* it sits in,
//! not just to a set of sanctioned spellings. A crate hands
//! [`assert_every_ignored_case_names_its_lane`] a table mapping each test
//! file's stem to the one lane prefix its `#[ignore]` reasons may open with,
//! and this module reads that crate's sources to say so. A stem the table does
//! not name is an ERROR — unknown, never lane-less — so an ignored case in a
//! new test file fails the crate's own guard until the table names the lane
//! that adopts it, and a prefix sanctioned for a different file is rejected
//! exactly as loudly as one that is not sanctioned at all.
//!
//! **What binds is the lane a case claims, not that a lane runs it.** Nothing
//! here reads a workflow: a stem in the table with no step behind it is a lane
//! that runs nothing, and that pairing is review-held. What this removes is the
//! opposite hazard — a case a lane picks up under a bar nobody assigned it.
//!
//! A second sweep catches the shapes the primary matcher cannot parse:
//! `#[cfg_attr(unix, ignore = "...")]` never starts a trimmed line with
//! `#[ignore`, and `#[test] #[ignore = "..."]` on one line puts the ignore
//! attribute second. Both are real `#[ignore]`s a test binary honors, so
//! failing to recognize either would let an unnamed-lane case run silently.
//! An unrecognized shape fails the check rather than being skipped — this
//! module does not parse every attribute grammar, so a line it cannot classify
//! as a plain, checkable `#[ignore = "..."]` is treated as unsafe rather than
//! passed through.
//!
//! **The enforcement lives here once, and the tables live with the crates
//! they describe.** Which stems a crate holds is that crate's own business and
//! changes in its own diffs; how a stem is checked is one walk, so a second
//! crate joining the lanes gains the guard rather than a copy of it.
//! [`LANE_PREFIXES_BY_PACKAGE`] is what keeps the tables from drifting apart:
//! each crate's guard asserts its table against the row named for it, and this
//! module asserts the rows against
//! [`LANE_IGNORE_PREFIXES`](crate::regression::LANE_IGNORE_PREFIXES).

use std::path::{Path, PathBuf};

/// Which lane prefixes each package's test files bind, by package name.
///
/// A package's own lane table is the authority on which *file* carries which
/// prefix; this is the authority on which prefixes the package uses at all.
/// The two directions are what the pair of assertions below close: every
/// prefix a crate binds is one a lane recognizes, and every prefix a lane
/// recognizes is bound by some crate.
pub const LANE_PREFIXES_BY_PACKAGE: &[(&str, &[&str])] = &[
    ("norn-fixtures", &["memory-lane case:", "soak-lane case:"]),
    ("norn-fs", &["memory-lane case:"]),
    (
        "norn-host",
        &["counter-lane case:", "memory-lane case:", "soak-lane case:"],
    ),
    ("norn-text", &["soak-lane case:"]),
];

/// Every `.rs` file under `manifest_dir`'s `tests/` directory.
///
/// The caller's own manifest directory is the root, so the walk is over that
/// package's sources and never over a path a test subject supplied.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: reads a crate's own test sources, from its own manifest directory.
fn test_sources(manifest_dir: &Path) -> Vec<PathBuf> {
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
    walk(&manifest_dir.join("tests"), &mut found);
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

/// Whether `reason` opens with the lane prefix `stem`'s file is bound to in
/// `lanes`.
///
/// `Err` describes what is wrong: a stem the table does not name, a sanctioned
/// prefix used in a file it is not bound to, or a reason with no lane prefix at
/// all.
fn check_ignore_reason(lanes: &[(&str, &str)], stem: &str, reason: &str) -> Result<(), String> {
    let Some(&(_, required)) = lanes.iter().find(|(s, _)| *s == stem) else {
        return Err(format!(
            "no lane is named for file stem `{stem}` in this crate's lane table. An ignored case \
             in a new test file means adding that file's stem there, in the same diff — an \
             unnamed stem is an error, not a case with no lane."
        ));
    };
    if reason.starts_with(required) {
        return Ok(());
    }
    if lanes.iter().any(|(_, prefix)| reason.starts_with(prefix)) {
        return Err(format!(
            "the ignore reason {reason:?} opens with a sanctioned lane prefix, but not the one \
             `{stem}.rs` is bound to (`{required}`) in this crate's lane table. A sanctioned \
             prefix in the wrong file is adopted by the wrong lane."
        ));
    }
    Err(format!(
        "the ignore reason {reason:?} opens with no lane prefix. `{stem}.rs` is bound to \
         `{required}` in this crate's lane table, because a CI step runs a suite's ignored cases \
         wholesale and would adopt this one."
    ))
}

/// Attribute-opening lines the primary matcher in [`ignore_attributes`] did
/// not claim, but which still mention `ignore` in a shape this checker does
/// not parse — as `(line number, line)`.
///
/// Scoped to lines whose trimmed text opens an attribute (`#[...]`), so a
/// doc comment or a string literal quoting `#[ignore]` for illustration —
/// this module's own unit tests do exactly that — is never mistaken for a real
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

/// **The lane-discipline walk.** Every `#[ignore]` under `manifest_dir`'s
/// `tests/` directory states the lane that adopts it, and states the one its
/// file is bound to in `lanes`.
///
/// A crate whose test sources hold no `#[ignore]` at all fails: the guard would
/// have checked nothing, and the lanes it protects would run nothing.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: reads a crate's own test sources, from its own manifest directory.
pub fn assert_every_ignored_case_names_its_lane(manifest_dir: &Path, lanes: &[(&str, &str)]) {
    let mut checked = 0usize;
    for path in test_sources(manifest_dir) {
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
            if let Err(problem) = check_ignore_reason(lanes, stem, reason) {
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
        "{}'s test sources hold no `#[ignore]` at all, so the guard checked nothing and the \
         lanes it protects run nothing",
        manifest_dir.display()
    );
}

/// **The containment.** `package`'s lane table binds exactly the prefixes
/// [`LANE_PREFIXES_BY_PACKAGE`] names for it.
///
/// The table and the row are one set spelled twice on purpose — the crate's
/// guard reads a table it owns, and this module reads a row it owns — so a
/// prefix added, removed, or respelled on one side fails here instead of
/// drifting.
pub fn assert_lane_prefixes_agree(package: &str, lanes: &[(&str, &str)]) {
    let mut here: Vec<&str> = lanes.iter().map(|(_, prefix)| *prefix).collect();
    here.sort_unstable();
    here.dedup();

    let Some(&(_, expected)) = LANE_PREFIXES_BY_PACKAGE
        .iter()
        .find(|(name, _)| *name == package)
    else {
        panic!(
            "`{package}` binds lane prefixes and LANE_PREFIXES_BY_PACKAGE names no row for it. A \
             package that runs ignored cases in a lane is named there, so the prefixes it uses \
             are checked against the ones a lane recognizes."
        );
    };
    let mut expected: Vec<&str> = expected.to_vec();
    expected.sort_unstable();

    assert_eq!(
        here, expected,
        "`{package}`'s lane table and its LANE_PREFIXES_BY_PACKAGE row have drifted. The two are \
         deliberate duplicates — the crate's guard reads its own table — so a lane added, \
         removed, or respelled in one lands in the other in the same diff."
    );
}

#[cfg(test)]
mod tests {
    use super::{
        LANE_PREFIXES_BY_PACKAGE, check_ignore_reason, ignore_attributes, reason,
        unrecognized_ignore_attribute_lines,
    };
    use crate::regression::LANE_IGNORE_PREFIXES;
    use std::collections::BTreeSet;

    const LANES: &[(&str, &str)] = &[
        ("memory", "memory-lane case:"),
        ("memory_soak", "soak-lane case:"),
    ];

    /// The rows are the whole set of lanes, spread across the packages that
    /// bind them: a prefix a lane recognizes and no package uses is a lane
    /// running nothing, and a prefix a package binds and no lane recognizes is
    /// an ignored case nothing adopts.
    #[test]
    fn every_recognized_lane_prefix_is_bound_by_some_package() {
        let bound: BTreeSet<&str> = LANE_PREFIXES_BY_PACKAGE
            .iter()
            .flat_map(|(_, prefixes)| prefixes.iter().copied())
            .collect();
        let recognized: BTreeSet<&str> = LANE_IGNORE_PREFIXES.iter().copied().collect();
        assert_eq!(bound, recognized);
    }

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
        assert!(check_ignore_reason(LANES, "memory", "memory-lane case: x").is_ok());
        assert!(check_ignore_reason(LANES, "memory_soak", "soak-lane case: x").is_ok());
    }

    #[test]
    fn a_sanctioned_prefix_in_the_wrong_file_is_rejected() {
        let problem = check_ignore_reason(LANES, "memory", "soak-lane case: x")
            .expect_err("a soak-lane reason does not belong to memory.rs");
        assert!(
            problem.contains("memory-lane case:"),
            "the error should name the prefix memory.rs is bound to: {problem}"
        );
    }

    #[test]
    fn an_unmapped_file_stem_is_rejected() {
        let problem = check_ignore_reason(LANES, "mystery", "soak-lane case: x")
            .expect_err("a stem with no table entry has nothing to check against");
        assert!(problem.contains("mystery"), "{problem}");
    }

    #[test]
    fn a_reason_with_no_lane_prefix_at_all_is_rejected() {
        let problem = check_ignore_reason(LANES, "memory", "flaky")
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
