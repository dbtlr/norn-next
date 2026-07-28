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
//! So the reason is not free text. Every ignored case in this crate's test
//! binaries opens its reason with one of the sanctioned lane prefixes below,
//! and this case reads the sources to say so. Adding a lane means adding a
//! prefix here, in the same diff as the workflow step that runs it.

use std::path::{Path, PathBuf};

/// The lane prefixes an `#[ignore]` reason may open with, and what each means.
///
/// - `soak-lane case:` — the nightly soak workflow's `--ignored` steps.
/// - `memory-lane case:` — the per-PR `memory invariant` job's `--ignored` step.
const LANE_PREFIXES: &[&str] = &["soak-lane case:", "memory-lane case:"];

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

#[test]
#[allow(clippy::disallowed_methods)] // Reads this crate's own test sources, from its own manifest directory.
fn every_ignored_case_names_the_lane_that_adopts_it() {
    let mut checked = 0usize;
    for path in test_sources() {
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        for (line, attribute) in ignore_attributes(&source) {
            let where_it_is = format!("{}:{line}", path.display());
            let reason = reason(attribute).unwrap_or_else(|| {
                panic!(
                    "{where_it_is}: `{attribute}` carries no reason. An ignored case states the \
                     lane that adopts it, because a CI step runs a suite's ignored cases \
                     wholesale."
                )
            });
            assert!(
                LANE_PREFIXES.iter().any(|p| reason.starts_with(p)),
                "{where_it_is}: the ignore reason {reason:?} opens with no lane prefix. \
                 One of {LANE_PREFIXES:?} must lead it, because a CI step runs a suite's \
                 ignored cases wholesale and would adopt this one."
            );
            checked += 1;
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
    use super::{ignore_attributes, reason};

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
}
