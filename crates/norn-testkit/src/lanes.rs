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
//! **The lane a case claims and the step that runs it are held to each other.**
//! [`assert_lane_steps_agree`] reads the workflows and pairs the two: a table
//! stem no step names is a lane that runs nothing, and a step naming a target
//! no table stem covers is a suite whose ignored cases a lane adopts outside
//! the walk above. Both directions fail, so a suite joins a lane in one diff or
//! not at all.
//!
//! A package with no lane table of its own would be invisible to that pairing —
//! nothing calls the guard for it, so neither direction has a caller — and its
//! ignored cases would be adopted wholesale with no prefix check at all. So
//! every package a step names is held against [`LANE_PREFIXES_BY_PACKAGE`] by
//! whichever guard reads the workflows: a package new to the lanes fails the
//! guards that already run until it is named there, which is what makes it
//! write the table its own guard then reads.
//!
//! Adoption **by kind** is what the pairing reads: `.github/scripts/lane-suite.sh`
//! is the one spelling that runs a target's ignored cases wholesale, so a step
//! invoking it is a step that adopts whatever `#[ignore]` that target holds. A
//! step that names its cases by filter adopts nothing wholesale — a stray
//! `#[ignore]` cannot fall into one — and is outside this pairing.
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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The script a CI step adopts a whole target's ignored cases through.
const LANE_SCRIPT: &str = "lane-suite.sh";

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

/// Every wholesale adoption a workflow declares, as `(package, test target)`.
///
/// The invocation is read as tokens rather than matched as a line, so a
/// wrapper in front of the script — the flake tripwire runs one — and harness
/// arguments behind it change nothing: the two tokens after the script are its
/// package and its target, which is the script's own argument order. A comment
/// line is not an invocation however it mentions the script, and the module
/// doc above mentions it in prose that would otherwise parse as one.
fn lane_steps(workflow: &str) -> Vec<(String, String)> {
    workflow
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let mut tokens = line.split_whitespace();
            tokens.find(|token| {
                *token == LANE_SCRIPT || token.ends_with(&format!("/{LANE_SCRIPT}"))
            })?;
            let package = tokens.next()?;
            let target = tokens.next()?;
            Some((package.to_string(), target.to_string()))
        })
        .collect()
}

/// The workflows directory above `manifest_dir`.
///
/// The walk is upward from the package's own manifest directory rather than
/// from a path a caller composed, so a package nested any depth under the
/// repository root finds the one workflow set that runs it.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: locates this repository's own workflow directory.
fn workflows_directory(manifest_dir: &Path) -> PathBuf {
    manifest_dir
        .ancestors()
        .map(|dir| dir.join(".github").join("workflows"))
        .find(|dir| dir.is_dir())
        .unwrap_or_else(|| {
            panic!(
                "no `.github/workflows` directory stands above {}, so the steps that adopt this \
                 package's ignored cases cannot be read",
                manifest_dir.display()
            )
        })
}

/// The packages a step adopts ignored cases from that
/// [`LANE_PREFIXES_BY_PACKAGE`] names no row for.
///
/// A package with no row has no lane table either — the two land together —
/// so its adopted suites are checked by nothing. Reported by every guard that
/// reads the workflows rather than by the row's own package, because the
/// package that is missing is the one with no guard running.
fn packages_outside_the_rows(adopting: &BTreeSet<String>) -> Vec<String> {
    adopting
        .iter()
        .filter(|named| {
            !LANE_PREFIXES_BY_PACKAGE
                .iter()
                .any(|(package, _)| package == *named)
        })
        .cloned()
        .collect()
}

/// **The pairing.** The targets CI adopts `package`'s ignored cases from are
/// exactly the file stems its lane table names, and every package CI adopts
/// ignored cases from is a package the lanes account for.
///
/// Three hazards close together here. A step naming a target the table does not
/// cover adopts that suite's `#[ignore]`s under a lane the walk above never
/// checked them against. A stem the table names and no step runs is a lane
/// that measures nothing — the ignored cases under it never execute, and the
/// pass-count assertion inside the script never gets the chance to say so. And
/// a step naming a package the rows do not know adopts a whole suite that no
/// guard reads at all, which is the first two hazards with nothing standing
/// where they would be caught.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: reads this repository's own workflow files.
pub fn assert_lane_steps_agree(manifest_dir: &Path, package: &str, lanes: &[(&str, &str)]) {
    let directory = workflows_directory(manifest_dir);
    let entries = std::fs::read_dir(&directory)
        .unwrap_or_else(|e| panic!("reading {} for workflows: {e}", directory.display()));

    let mut workflows = 0usize;
    let mut adopted: BTreeSet<String> = BTreeSet::new();
    let mut adopting: BTreeSet<String> = BTreeSet::new();
    for entry in entries {
        let path = entry.expect("a directory entry").path();
        if !path.extension().is_some_and(|e| e == "yml" || e == "yaml") {
            continue;
        }
        workflows += 1;
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        for (named, target) in lane_steps(&text) {
            if named == package {
                adopted.insert(target);
            }
            adopting.insert(named);
        }
    }
    assert!(
        workflows > 0,
        "{} holds no workflow, so nothing was read",
        directory.display()
    );

    let unaccounted = packages_outside_the_rows(&adopting);
    assert!(
        unaccounted.is_empty(),
        "CI runs `{LANE_SCRIPT}` for {unaccounted:?}, and LANE_PREFIXES_BY_PACKAGE names no row \
         for them. A package with no row has no lane table, so nothing checks the `#[ignore]` \
         reasons the step adopts wholesale. The row and the package's own `tests/lanes.rs` land \
         with the step that adopts it."
    );

    let tabled: BTreeSet<String> = lanes.iter().map(|(stem, _)| (*stem).to_string()).collect();
    assert_eq!(
        adopted, tabled,
        "`{package}`'s lane table and the CI steps that adopt its ignored cases have drifted. The \
         steps run `{LANE_SCRIPT}` against these targets: {adopted:?}; the table names these file \
         stems: {tabled:?}. A target the table does not name has its `#[ignore]`s adopted under a \
         lane nothing checked them against, and a stem no step runs is a lane that measures \
         nothing."
    );
}

#[cfg(test)]
mod tests {
    use super::{
        LANE_PREFIXES_BY_PACKAGE, check_ignore_reason, ignore_attributes, lane_steps,
        packages_outside_the_rows, reason, unrecognized_ignore_attribute_lines,
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

    /// **A package new to the lanes is caught by the guards already running.**
    /// A step adopting a package the rows do not name is the one adoption no
    /// per-package guard can see: that package has no `tests/lanes.rs`, so
    /// neither direction of the pairing has a caller for it.
    #[test]
    fn a_package_no_row_names_is_reported_as_unaccounted() {
        let adopting = ["norn-host", "norn-store"]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            packages_outside_the_rows(&adopting),
            vec!["norn-store".to_string()]
        );
    }

    #[test]
    fn the_packages_the_rows_name_are_accounted_for() {
        let adopting = LANE_PREFIXES_BY_PACKAGE
            .iter()
            .map(|(package, _)| (*package).to_string())
            .collect::<BTreeSet<_>>();
        assert!(packages_outside_the_rows(&adopting).is_empty());
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

    /// A step reads as its package and its target whatever stands around the
    /// script: a wrapper in front of it, harness arguments behind it, and the
    /// YAML that carries the line.
    #[test]
    fn a_wrapped_invocation_reads_as_its_package_and_target() {
        let workflow = "        run: .github/scripts/flake-tripwire.sh \
                        .github/scripts/lane-suite.sh norn-host memory --nocapture\n";
        assert_eq!(
            lane_steps(workflow),
            vec![("norn-host".to_string(), "memory".to_string())]
        );
    }

    #[test]
    fn a_bare_invocation_reads_as_its_package_and_target() {
        let workflow = "        run: .github/scripts/lane-suite.sh norn-text frontmatter_cost\n";
        assert_eq!(
            lane_steps(workflow),
            vec![("norn-text".to_string(), "frontmatter_cost".to_string())]
        );
    }

    /// **A comment is prose, not a step.** Both workflows explain the script
    /// above the jobs that run it, and a comment read as an invocation would
    /// bind a lane to whatever two words followed the sentence.
    #[test]
    fn a_comment_mentioning_the_script_is_not_a_step() {
        let workflow = "# `lane-suite.sh` runs a suite's ignored cases and fails a step that \
                        measured nothing.\n";
        assert!(lane_steps(workflow).is_empty());
    }

    #[test]
    fn a_line_that_names_no_script_is_not_a_step() {
        let workflow = "        run: cargo test --locked --workspace\n";
        assert!(lane_steps(workflow).is_empty());
    }
}
